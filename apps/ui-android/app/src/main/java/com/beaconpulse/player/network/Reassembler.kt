package com.beaconpulse.player.network

import java.util.TreeMap
import java.util.HashMap

data class FrameKey(val timestampUs: Long, val displayId: Byte)

data class CompleteFrame(
    val timestampUs: Long,
    val displayId: Byte,
    val isKeyframe: Boolean,
    val width: Short,
    val height: Short,
    val data: ByteArray
)

class Reassembler {
    private val frags = HashMap<FrameKey, TreeMap<Short, ByteArray>>()
    private val expectedTotals = HashMap<FrameKey, Short>()
    private val keyframeFlags = HashMap<FrameKey, Boolean>()

    fun feed(pkt: RtpPacket): CompleteFrame? {
        val key = FrameKey(pkt.timestampUs, pkt.displayId)
        
        // Ignore FEC/parity packets to keep reassembly simple and fast
        if ((pkt.flags.toInt() and RtpPacket.FLAG_PARITY.toInt()) != 0) {
            return null
        }

        val tree = frags.getOrPut(key) { TreeMap() }
        tree[pkt.fragIdx] = pkt.payload
        expectedTotals[key] = pkt.fragTotal
        if (pkt.isKeyframe()) {
            keyframeFlags[key] = true
        }

        val total = pkt.fragTotal
        if (tree.size == total.toInt()) {
            // Frame is complete! Assemble it.
            val totalSize = tree.values.sumOf { it.size }
            val assembled = ByteArray(totalSize)
            var offset = 0
            for (part in tree.values) {
                System.arraycopy(part, 0, assembled, offset, part.size)
                offset += part.size
            }
            val isKey = keyframeFlags[key] ?: false
            
            // Cleanup this frame's tracking
            frags.remove(key)
            expectedTotals.remove(key)
            keyframeFlags.remove(key)

            // Evict any stale frames (older than 2 seconds) to prevent memory leak
            val currentTs = pkt.timestampUs
            val staleKeys = frags.keys.filter { Math.abs(currentTs - it.timestampUs) > 2_000_000 }
            for (sk in staleKeys) {
                frags.remove(sk)
                expectedTotals.remove(sk)
                keyframeFlags.remove(sk)
            }

            return CompleteFrame(pkt.timestampUs, pkt.displayId, isKey, pkt.width, pkt.height, assembled)
        }
        return null
    }
}
