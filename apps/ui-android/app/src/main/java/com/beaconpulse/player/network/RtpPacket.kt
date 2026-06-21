package com.beaconpulse.player.network

import java.nio.ByteBuffer
import java.nio.ByteOrder

class RtpPacket(
    val flags: Byte,
    val displayId: Byte,
    val seq: Short,
    val timestampUs: Long,
    val width: Short,
    val height: Short,
    val fragIdx: Short,
    val fragTotal: Short,
    val payload: ByteArray
) {
    fun isKeyframe(): Boolean {
        return (flags.toInt() and FLAG_KEYFRAME.toInt()) != 0
    }

    companion object {
        const val MAGIC = 0x4C414E53 // "LANS"
        const val VERSION: Byte = 1
        const val HEADER_SIZE = 27

        const val FLAG_KEYFRAME: Byte = 0x01
        const val FLAG_FRAG_END: Byte = 0x02
        const val FLAG_PARITY: Byte = 0x04

        fun fromBytes(data: ByteArray, len: Int): RtpPacket? {
            if (len < HEADER_SIZE) return null
            try {
                val buffer = ByteBuffer.wrap(data, 0, len).order(ByteOrder.LITTLE_ENDIAN)
                val magic = buffer.int
                if (magic != MAGIC) return null
                val version = buffer.get()
                if (version != VERSION) return null
                val flags = buffer.get()
                val displayId = buffer.get()
                val seq = buffer.short
                val timestampUs = buffer.long
                val width = buffer.short
                val height = buffer.short
                val fragIdx = buffer.short
                val fragTotal = buffer.short
                val dataLen = buffer.short.toInt() and 0xFFFF
                
                if (len < HEADER_SIZE + dataLen) return null
                val payload = ByteArray(dataLen)
                System.arraycopy(data, HEADER_SIZE, payload, 0, dataLen)
                return RtpPacket(flags, displayId, seq, timestampUs, width, height, fragIdx, fragTotal, payload)
            } catch (e: Exception) {
                e.printStackTrace()
                return null
            }
        }
    }
}
