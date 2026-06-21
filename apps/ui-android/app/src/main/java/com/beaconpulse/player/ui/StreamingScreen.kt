package com.beaconpulse.player.ui

import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.changedToDown
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import com.beaconpulse.player.network.CompleteFrame
import com.beaconpulse.player.network.H264Decoder
import com.beaconpulse.player.network.MultiIpConnector
import com.beaconpulse.player.network.NetworkClient
import org.json.JSONObject

@Composable
fun StreamingScreen(
    hostIps: List<String>,
    controlPort: Int,
    pairingCode: String?,
    onDisconnect: () -> Unit
) {
    val context = LocalContext.current
    var showMenu by remember { mutableStateOf(false) }
    var connectionStatus by remember { mutableStateOf("Connecting to host...") }
    var statsText by remember { mutableStateOf("Latency: --ms  |  FPS: --") }
    
    var viewSize by remember { mutableStateOf(androidx.compose.ui.unit.IntSize(1920, 1080)) }
    
    // Hold references to native network and decoder components
    var activeClient by remember { mutableStateOf<NetworkClient?>(null) }
    var decoder by remember { mutableStateOf<H264Decoder?>(null) }
    var connector by remember { mutableStateOf<MultiIpConnector?>(null) }

    // Measure FPS locally
    var frameCount by remember { mutableStateOf(0) }
    var lastFpsCalculationTime by remember { mutableStateOf(System.currentTimeMillis()) }

    // Cleanup on dispose
    DisposableEffect(Unit) {
        onDispose {
            connector?.close()
            activeClient?.close()
            decoder?.release()
        }
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black)
            .onSizeChanged { viewSize = it }
            .pointerInput(viewSize) {
                // Intercept touch events on screen and map them to remote mouse events
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent()
                        val change = event.changes.firstOrNull() ?: continue
                        val pos = change.position
                        
                        val client = activeClient ?: continue
                        val width = viewSize.width
                        val height = viewSize.height
                        
                        // Normalize positions to floats in 0.0 .. 1.0
                        val normX = (pos.x / width.toFloat()).coerceIn(0f, 1f)
                        val normY = (pos.y / height.toFloat()).coerceIn(0f, 1f)
                        
                        if (change.changedToDown()) {
                            // Left Mouse Button Down
                            val clickEvent = JSONObject().apply {
                                put("kind", "mouse_button")
                                put("button", 1) // 1 = Left Click
                                put("pressed", true)
                                put("x", normX)
                                put("y", normY)
                                put("viewport_w", width)
                                put("viewport_h", height)
                                put("display_id", 0)
                            }
                            client.sendInputEvent(clickEvent)
                            change.consume()
                        } else if (change.changedToUp()) {
                            // Left Mouse Button Up
                            val clickEvent = JSONObject().apply {
                                put("kind", "mouse_button")
                                put("button", 1)
                                put("pressed", false)
                                put("x", normX)
                                put("y", normY)
                                put("viewport_w", width)
                                put("viewport_h", height)
                                put("display_id", 0)
                            }
                            client.sendInputEvent(clickEvent)
                            change.consume()
                        } else if (change.pressed) {
                            // Mouse Move / Drag
                            val moveEvent = JSONObject().apply {
                                put("kind", "mouse_move")
                                put("x", normX)
                                put("y", normY)
                                put("viewport_w", width)
                                put("viewport_h", height)
                                put("display_id", 0)
                            }
                            client.sendInputEvent(moveEvent)
                            change.consume()
                        }
                    }
                }
            }
    ) {
        // Native SurfaceView to receive hardware decoded frames
        AndroidView(
            factory = { ctx ->
                SurfaceView(ctx).apply {
                    holder.addCallback(object : SurfaceHolder.Callback {
                        override fun surfaceCreated(holder: SurfaceHolder) {
                            val activeDecoder = H264Decoder(holder.surface)
                            decoder = activeDecoder

                            // Start multi-IP parallel connection manager
                            val conn = MultiIpConnector(
                                ips = hostIps,
                                port = controlPort,
                                pairingCode = pairingCode,
                                callback = object : NetworkClient.NetworkCallback {
                                    override fun onConnected(actualUdpPort: Int) {
                                        connectionStatus = "Connected"
                                    }

                                    override fun onFrameReady(frame: CompleteFrame) {
                                        activeDecoder.configure(frame.width.toInt(), frame.height.toInt())
                                        activeDecoder.decode(frame.data, frame.timestampUs)
                                        
                                        // Update FPS stats locally
                                        frameCount++
                                        val now = System.currentTimeMillis()
                                        if (now - lastFpsCalculationTime >= 1000) {
                                            val fps = frameCount
                                            statsText = "Latency: --ms  |  FPS: $fps"
                                            frameCount = 0
                                            lastFpsCalculationTime = now
                                        }
                                    }

                                    override fun onDisconnected(reason: String) {
                                        connectionStatus = "Disconnected"
                                        Toast.makeText(context, "Disconnected: $reason", Toast.LENGTH_LONG).show()
                                        onDisconnect()
                                    }

                                    override fun getPairingCode(): String? {
                                        return pairingCode
                                    }
                                }
                            )
                            connector = conn
                            conn.start(
                                onSuccess = { client ->
                                    activeClient = client
                                },
                                onFailure = { errorMsg ->
                                    connectionStatus = "Failed"
                                    Toast.makeText(context, errorMsg, Toast.LENGTH_LONG).show()
                                    onDisconnect()
                                }
                            )
                        }

                        override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

                        override fun surfaceDestroyed(holder: SurfaceHolder) {
                            connector?.close()
                            activeClient?.close()
                            decoder?.release()
                            decoder = null
                            activeClient = null
                            connector = null
                        }
                    })
                }
            },
            modifier = Modifier.fillMaxSize()
        )

        // Overlay status indicators
        if (connectionStatus != "Connected") {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(Color(0xDD0F172A)),
                contentAlignment = Alignment.Center
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    CircularProgressIndicator(color = Color(0xFF6366F1))
                    Spacer(modifier = Modifier.height(16.dp))
                    Text(
                        text = connectionStatus,
                        color = Color.White,
                        fontSize = 18.sp,
                        fontWeight = FontWeight.Bold
                    )
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = "Trying IPs: " + hostIps.joinToString(", "),
                        color = Color.Gray,
                        fontSize = 12.sp
                    )
                }
            }
        } else {
            // Live Stats Overlay (Top Left)
            Column(
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(16.dp)
                    .background(Color(0x80000000), RoundedCornerShape(8.dp))
                    .padding(8.dp)
            ) {
                Text(statsText, color = Color.Green, fontSize = 11.sp)
            }
        }

        // Overlay menu trigger button (Bottom Right)
        Box(
            modifier = Modifier
                .align(Alignment.BottomEnd)
                .padding(16.dp)
        ) {
            FloatingActionButton(
                onClick = { showMenu = !showMenu },
                containerColor = Color(0xFF6366F1),
                contentColor = Color.White
            ) {
                Text(if (showMenu) "❌" else "⚙️", fontSize = 18.sp)
            }
        }

        // Settings / Disconnect Menu Overlay
        if (showMenu) {
            Card(
                colors = CardDefaults.cardColors(containerColor = Color(0xEE1E293B)),
                shape = RoundedCornerShape(16.dp),
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .fillMaxWidth()
                    .padding(horizontal = 24.dp, vertical = 80.dp)
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(16.dp),
                    horizontalArrangement = Arrangement.SpaceEvenly,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    // Send Request Keyframe
                    Button(
                        onClick = {
                            activeClient?.sendInputEvent(JSONObject().apply {
                                put("type", "request_keyframe")
                            })
                            Toast.makeText(context, "Requested Keyframe", Toast.LENGTH_SHORT).show()
                        },
                        colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF334155))
                    ) {
                        Text("🔄 Refresh", color = Color.White, fontSize = 12.sp)
                    }

                    // Touch mode indicator (Direct screen touch input is active)
                    Button(
                        onClick = {},
                        enabled = false,
                        colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF334155))
                    ) {
                        Text("🖱️ Touch Mode", color = Color.White, fontSize = 12.sp)
                    }

                    // Disconnect button
                    Button(
                        onClick = {
                            connector?.close()
                            activeClient?.close()
                            decoder?.release()
                            onDisconnect()
                        },
                        colors = ButtonDefaults.buttonColors(containerColor = Color.Red)
                    ) {
                        Text("Disconnect", color = Color.White, fontSize = 12.sp, fontWeight = FontWeight.Bold)
                    }
                }
            }
        }
    }
}
