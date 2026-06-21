package com.beaconpulse.player.network

import android.media.MediaCodec
import android.media.MediaFormat
import android.view.Surface
import java.nio.ByteBuffer

class H264Decoder(private val surface: Surface) {
    private var codec: MediaCodec? = null
    private var isConfigured = false
    private var currentWidth = 0
    private var currentHeight = 0

    fun configure(width: Int, height: Int) {
        if (isConfigured && currentWidth == width && currentHeight == height) {
            return
        }
        
        release()
        
        try {
            val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
            // Request low-latency decoding if supported by the OS (Android 11+)
            if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.R) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            
            codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            codec?.configure(format, surface, null, 0)
            codec?.start()
            
            currentWidth = width
            currentHeight = height
            isConfigured = true
        } catch (e: Exception) {
            e.printStackTrace()
            isConfigured = false
        }
    }

    fun decode(data: ByteArray, timestampUs: Long) {
        val currentCodec = codec ?: return
        if (!isConfigured) return
        
        try {
            val inputBufferIndex = currentCodec.dequeueInputBuffer(5000)
            if (inputBufferIndex >= 0) {
                val inputBuffer = currentCodec.getInputBuffer(inputBufferIndex)
                if (inputBuffer != null) {
                    inputBuffer.clear()
                    inputBuffer.put(data)
                    currentCodec.queueInputBuffer(inputBufferIndex, 0, data.size, timestampUs, 0)
                }
            }

            val bufferInfo = MediaCodec.BufferInfo()
            var outputBufferIndex = currentCodec.dequeueOutputBuffer(bufferInfo, 0)
            while (outputBufferIndex >= 0) {
                // Render the buffer content directly to the surface
                currentCodec.releaseOutputBuffer(outputBufferIndex, true)
                outputBufferIndex = currentCodec.dequeueOutputBuffer(bufferInfo, 0)
            }
        } catch (e: Exception) {
            e.printStackTrace()
        }
    }

    fun release() {
        try {
            codec?.stop()
            codec?.release()
        } catch (e: Exception) {
            // ignore
        }
        codec = null
        isConfigured = false
    }
}
