package com.beaconpulse.player.network

import android.util.Base64
import org.json.JSONObject
import java.io.BufferedReader
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.UUID
import java.util.concurrent.Executors
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

class NetworkClient(
    val hostIp: String,
    val controlPort: Int,
    private val callback: NetworkCallback
) {
    interface NetworkCallback {
        fun onConnected(actualUdpPort: Int)
        fun onFrameReady(frame: CompleteFrame)
        fun onDisconnected(reason: String)
        fun getPairingCode(): String?
    }

    private var tcpSocket: Socket? = null
    private var udpSocket: DatagramSocket? = null
    @Volatile
    private var isRunning = false
    private val reassembler = Reassembler()
    private val clientExecutor = Executors.newSingleThreadExecutor()
    private val writeExecutor = Executors.newSingleThreadExecutor()

    fun start() {
        isRunning = true
        clientExecutor.submit {
            runConnection()
        }
    }

    private fun runConnection() {
        try {
            // 1. Bind local UDP socket to get actual UDP port
            val udp = DatagramSocket(0)
            udp.soTimeout = 500
            udpSocket = udp
            val actualUdpPort = udp.localPort

            // 2. Connect to PC via TCP
            val tcp = Socket(hostIp, controlPort)
            tcp.tcpNoDelay = true
            tcpSocket = tcp

            val writer = BufferedWriter(OutputStreamWriter(tcp.getOutputStream(), "UTF-8"))
            val reader = BufferedReader(InputStreamReader(tcp.getInputStream(), "UTF-8"))

            // 3. Send JoinRequest
            val clientId = UUID.randomUUID().toString()
            val hostname = android.os.Build.MODEL
            val joinRequest = JSONObject().apply {
                put("type", "join_request")
                put("client_id", clientId)
                put("display_name", hostname)
                put("version", "1.1.0")
                put("udp_port", actualUdpPort)
            }
            writer.write(joinRequest.toString() + "\n")
            writer.flush()

            // 4. Read response
            val firstLine = reader.readLine() ?: throw Exception("Host closed connection during handshake")
            val firstMsg = JSONObject(firstLine)
            val type = firstMsg.optString("type")
            
            if (type == "join_accepted") {
                // Accepted directly
            } else if (type == "pairing_required") {
                val challenge = firstMsg.getString("challenge")
                val code = callback.getPairingCode() ?: ""
                val hmacStr = computeHmacSha256(code, challenge)
                val reply = JSONObject().apply {
                    put("type", "pairing_code")
                    put("hmac", hmacStr)
                }
                writer.write(reply.toString() + "\n")
                writer.flush()

                // Read final accept/reject
                val secondLine = reader.readLine() ?: throw Exception("Host closed after HMAC verification")
                val secondMsg = JSONObject(secondLine)
                if (secondMsg.optString("type") != "join_accepted") {
                    throw Exception("Pairing rejected: " + secondMsg.optString("reason", "Unknown reason"))
                }
            } else if (type == "join_rejected") {
                throw Exception("Connection rejected by host: " + firstMsg.optString("reason", "Unknown reason"))
            } else {
                throw Exception("Unexpected handshake message: $firstLine")
            }

            callback.onConnected(actualUdpPort)

            // Start UDP receive loop
            Thread {
                runUdpReceive(udp)
            }.start()

            // Read TCP stream for disconnect signals
            while (isRunning) {
                val line = reader.readLine() ?: break
                val msg = JSONObject(line)
                when (msg.optString("type")) {
                    "disconnect" -> {
                        callback.onDisconnected(msg.optString("reason", "Host disconnected"))
                        break
                    }
                    "stream_stopped" -> {
                        callback.onDisconnected("Stream stopped: " + msg.optString("reason", "No reason"))
                        break
                    }
                }
            }

        } catch (e: Exception) {
            if (isRunning) {
                callback.onDisconnected(e.message ?: "Connection error")
            }
        } finally {
            close()
        }
    }

    private fun runUdpReceive(socket: DatagramSocket) {
        val buffer = ByteArray(2048)
        val packet = DatagramPacket(buffer, buffer.size)
        while (isRunning) {
            try {
                socket.receive(packet)
                
                // Check if RTCP probe packet
                if (packet.length >= 16) {
                    val rtcpMagic = ByteBuffer.wrap(buffer, 0, 4).order(ByteOrder.LITTLE_ENDIAN).int
                    if (rtcpMagic == 0x4C524350) { // "LRCP"
                        val type = buffer[4]
                        if (type == 1.toByte()) { // Probe
                            // Echo back Ack
                            val ack = ByteArray(16)
                            ByteBuffer.wrap(ack).order(ByteOrder.LITTLE_ENDIAN).putInt(0x4C524350)
                            ack[4] = 2.toByte() // ACK
                            System.arraycopy(buffer, 8, ack, 8, 8)
                            val replyPacket = DatagramPacket(ack, ack.size, packet.address, packet.port)
                            socket.send(replyPacket)
                        }
                        continue
                    }
                }

                val rtp = RtpPacket.fromBytes(buffer, packet.length)
                if (rtp != null) {
                    val frame = reassembler.feed(rtp)
                    if (frame != null) {
                        callback.onFrameReady(frame)
                    }
                }
            } catch (e: SocketTimeoutException) {
                // Ignore timeout and check if still running
            } catch (e: Exception) {
                if (isRunning) {
                    e.printStackTrace()
                }
            }
        }
    }

    fun sendInputEvent(event: JSONObject) {
        if (!isRunning) return
        writeExecutor.submit {
            try {
                val socket = tcpSocket ?: return@submit
                val writer = BufferedWriter(OutputStreamWriter(socket.getOutputStream(), "UTF-8"))
                val inputEvent = JSONObject().apply {
                    put("type", "input_event")
                    put("event", event)
                }
                writer.write(inputEvent.toString() + "\n")
                writer.flush()
            } catch (e: Exception) {
                e.printStackTrace()
            }
        }
    }

    fun close() {
        isRunning = false
        try {
            tcpSocket?.close()
        } catch (e: Exception) {}
        tcpSocket = null
        try {
            udpSocket?.close()
        } catch (e: Exception) {}
        udpSocket = null
    }

    private fun computeHmacSha256(key: String, dataB64: String): String {
        val challengeBytes = Base64.decode(dataB64, Base64.DEFAULT)
        val mac = Mac.getInstance("HmacSHA256")
        val secretKey = SecretKeySpec(key.toByteArray(charset("UTF-8")), "HmacSHA256")
        mac.init(secretKey)
        val result = mac.doFinal(challengeBytes)
        return Base64.encodeToString(result, Base64.NO_WRAP)
    }
}

class MultiIpConnector(
    private val ips: List<String>,
    private val port: Int,
    private val pairingCode: String?,
    private val callback: NetworkClient.NetworkCallback
) {
    private var successfulClient: NetworkClient? = null
    private val activeClients = mutableListOf<NetworkClient>()
    private val lock = Any()
    private var failedCount = 0
    @Volatile
    private var isStarted = false

    fun start(onSuccess: (NetworkClient) -> Unit, onFailure: (String) -> Unit) {
        if (ips.isEmpty()) {
            onFailure("No IP addresses available to connect")
            return
        }
        isStarted = true
        for (ip in ips) {
            val client = NetworkClient(ip, port, object : NetworkClient.NetworkCallback {
                override fun onConnected(actualUdpPort: Int) {
                    synchronized(lock) {
                        if (successfulClient == null) {
                            successfulClient = activeClients.find { it.hostIp == ip }
                            if (successfulClient != null) {
                                callback.onConnected(actualUdpPort)
                                onSuccess(successfulClient!!)
                                // Close all other clients
                                for (c in activeClients) {
                                    if (c != successfulClient) {
                                        c.close()
                                    }
                                }
                            }
                        }
                    }
                }

                override fun onFrameReady(frame: CompleteFrame) {
                    callback.onFrameReady(frame)
                }

                override fun onDisconnected(reason: String) {
                    synchronized(lock) {
                        failedCount++
                        if (failedCount == ips.size && successfulClient == null) {
                            onFailure("Failed to connect to any of the host IP addresses: " + ips.joinToString(", "))
                        }
                        if (successfulClient != null && successfulClient?.hostIp == ip) {
                            callback.onDisconnected(reason)
                        }
                    }
                }

                override fun getPairingCode(): String? {
                    return pairingCode
                }
            })
            synchronized(lock) {
                if (isStarted) {
                    activeClients.add(client)
                    client.start()
                }
            }
        }
    }

    fun close() {
        synchronized(lock) {
            isStarted = false
            for (c in activeClients) {
                c.close()
            }
            activeClients.clear()
            successfulClient = null
        }
    }
}
