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
import androidx.compose.ui.geometry.Offset
import kotlin.math.sqrt
import kotlin.math.abs

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
    var isTrackpadMode by remember { mutableStateOf(true) }
    var virtualCursorX by remember { mutableStateOf(0.5f) }
    var virtualCursorY by remember { mutableStateOf(0.5f) }
    
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
            .pointerInput(viewSize, isTrackpadMode) {
                awaitPointerEventScope {
                    var lastTapTime = 0L
                    var lastTapPos = Offset.Zero
                    var isDoubleTapAndHold = false
                    
                    var isLeftMouseDown = false
                    var isRightMouseDown = false
                    
                    var touchDownTime = 0L
                    var touchDownPos = Offset.Zero
                    var isLongPressTriggered = false
                    
                    // Track multi-finger states
                    var isMultiTouch = false
                    var twoFingerScrollStarted = false
                    var twoFingerTapPossible = false
                    var twoFingerDownTime = 0L
                    var twoFingerStartPos1 = Offset.Zero
                    var twoFingerStartPos2 = Offset.Zero

                    while (true) {
                        val event = awaitPointerEvent()
                        val client = activeClient ?: continue
                        val width = viewSize.width
                        val height = viewSize.height
                        
                        val pressedPointers = event.changes.filter { it.pressed }
                        val numFingers = pressedPointers.size

                        if (numFingers == 0) {
                            // All fingers lifted
                            val releasedPointers = event.changes.filter { !it.pressed && it.previousPressed }
                            if (releasedPointers.isNotEmpty()) {
                                val change = releasedPointers.first()
                                val pos = change.position
                                val normX = (pos.x / width.toFloat()).coerceIn(0f, 1f)
                                val normY = (pos.y / height.toFloat()).coerceIn(0f, 1f)

                                if (isMultiTouch) {
                                    // Multi-touch sequence ended
                                    if (twoFingerTapPossible && !twoFingerScrollStarted) {
                                        val duration = System.currentTimeMillis() - twoFingerDownTime
                                        if (duration < 250) {
                                            // Send Right Click
                                            val rx = if (isTrackpadMode) virtualCursorX else normX
                                            val ry = if (isTrackpadMode) virtualCursorY else normY
                                            val clickDown = JSONObject().apply {
                                                put("kind", "mouse_button")
                                                put("button", 1) // 1 = Right click
                                                put("pressed", true)
                                                put("x", rx)
                                                put("y", ry)
                                                put("viewport_w", width)
                                                put("viewport_h", height)
                                                put("display_id", 0)
                                            }
                                            val clickUp = JSONObject().apply {
                                                put("kind", "mouse_button")
                                                put("button", 1)
                                                put("pressed", false)
                                                put("x", rx)
                                                put("y", ry)
                                                put("viewport_w", width)
                                                put("viewport_h", height)
                                                put("display_id", 0)
                                            }
                                            client.sendInputEvent(clickDown)
                                            client.sendInputEvent(clickUp)
                                        }
                                    }
                                    isMultiTouch = false
                                    twoFingerScrollStarted = false
                                    twoFingerTapPossible = false
                                } else {
                                    // Single finger lifted
                                    if (isLongPressTriggered) {
                                        // Release Right Mouse Button if direct touch
                                        if (!isTrackpadMode && isRightMouseDown) {
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
                                            isRightMouseDown = false
                                        }
                                        isLongPressTriggered = false
                                    } else {
                                        if (isTrackpadMode) {
                                            if (isDoubleTapAndHold) {
                                                // Release Left mouse
                                                if (isLeftMouseDown) {
                                                    val clickEvent = JSONObject().apply {
                                                        put("kind", "mouse_button")
                                                        put("button", 0) // Left Mouse Button Up
                                                        put("pressed", false)
                                                        put("x", virtualCursorX)
                                                        put("y", virtualCursorY)
                                                        put("viewport_w", width)
                                                        put("viewport_h", height)
                                                        put("display_id", 0)
                                                    }
                                                    client.sendInputEvent(clickEvent)
                                                    isLeftMouseDown = false
                                                }
                                                isDoubleTapAndHold = false
                                            } else {
                                                // Check for tap / click
                                                val duration = System.currentTimeMillis() - touchDownTime
                                                val dx = pos.x - touchDownPos.x
                                                val dy = pos.y - touchDownPos.y
                                                val distance = sqrt(dx * dx + dy * dy)
                                                if (duration < 250 && distance < 20f) {
                                                    val now = System.currentTimeMillis()
                                                    val tapDx = pos.x - lastTapPos.x
                                                    val tapDy = pos.y - lastTapPos.y
                                                    val tapDistance = sqrt(tapDx * tapDx + tapDy * tapDy)
                                                    if (now - lastTapTime < 300 && tapDistance < 50f) {
                                                        // Double click
                                                        val click1D = JSONObject().apply {
                                                            put("kind", "mouse_button")
                                                            put("button", 0)
                                                            put("pressed", true)
                                                            put("x", virtualCursorX)
                                                            put("y", virtualCursorY)
                                                            put("viewport_w", width)
                                                            put("viewport_h", height)
                                                            put("display_id", 0)
                                                        }
                                                        val click1U = JSONObject().apply {
                                                            put("kind", "mouse_button")
                                                            put("button", 0)
                                                            put("pressed", false)
                                                            put("x", virtualCursorX)
                                                            put("y", virtualCursorY)
                                                            put("viewport_w", width)
                                                            put("viewport_h", height)
                                                            put("display_id", 0)
                                                        }
                                                        val click2D = JSONObject().apply {
                                                            put("kind", "mouse_button")
                                                            put("button", 0)
                                                            put("pressed", true)
                                                            put("x", virtualCursorX)
                                                            put("y", virtualCursorY)
                                                            put("viewport_w", width)
                                                            put("viewport_h", height)
                                                            put("display_id", 0)
                                                        }
                                                        val click2U = JSONObject().apply {
                                                            put("kind", "mouse_button")
                                                            put("button", 0)
                                                            put("pressed", false)
                                                            put("x", virtualCursorX)
                                                            put("y", virtualCursorY)
                                                            put("viewport_w", width)
                                                            put("viewport_h", height)
                                                            put("display_id", 0)
                                                        }
                                                        client.sendInputEvent(click1D)
                                                        client.sendInputEvent(click1U)
                                                        client.sendInputEvent(click2D)
                                                        client.sendInputEvent(click2U)
                                                        lastTapTime = 0L
                                                    } else {
                                                        // Single click
                                                        val clickDown = JSONObject().apply {
                                                            put("kind", "mouse_button")
                                                            put("button", 0) // Left Click Down
                                                            put("pressed", true)
                                                            put("x", virtualCursorX)
                                                            put("y", virtualCursorY)
                                                            put("viewport_w", width)
                                                            put("viewport_h", height)
                                                            put("display_id", 0)
                                                        }
                                                        val clickUp = JSONObject().apply {
                                                            put("kind", "mouse_button")
                                                            put("button", 0) // Left Click Up
                                                            put("pressed", false)
                                                            put("x", virtualCursorX)
                                                            put("y", virtualCursorY)
                                                            put("viewport_w", width)
                                                            put("viewport_h", height)
                                                            put("display_id", 0)
                                                        }
                                                        client.sendInputEvent(clickDown)
                                                        client.sendInputEvent(clickUp)
                                                        lastTapTime = now
                                                        lastTapPos = pos
                                                    }
                                                }
                                            }
                                        } else {
                                            // Direct touch: send left mouse up
                                            if (isLeftMouseDown) {
                                                val clickEvent = JSONObject().apply {
                                                    put("kind", "mouse_button")
                                                    put("button", 0) // Left Mouse Button Up (0)
                                                    put("pressed", false)
                                                    put("x", normX)
                                                    put("y", normY)
                                                    put("viewport_w", width)
                                                    put("viewport_h", height)
                                                    put("display_id", 0)
                                                }
                                                client.sendInputEvent(clickEvent)
                                                isLeftMouseDown = false
                                            }
                                        }
                                    }
                                }
                                change.consume()
                            }
                        } else if (numFingers == 1) {
                            val change = pressedPointers.first()
                            val pos = change.position
                            val normX = (pos.x / width.toFloat()).coerceIn(0f, 1f)
                            val normY = (pos.y / height.toFloat()).coerceIn(0f, 1f)

                            if (change.changedToDown()) {
                                touchDownTime = System.currentTimeMillis()
                                touchDownPos = pos
                                isLongPressTriggered = false

                                if (isTrackpadMode) {
                                    val now = System.currentTimeMillis()
                                    val tapDx = pos.x - lastTapPos.x
                                    val tapDy = pos.y - lastTapPos.y
                                    val tapDistance = sqrt(tapDx * tapDx + tapDy * tapDy)
                                    if (now - lastTapTime < 300 && tapDistance < 50f) {
                                        isDoubleTapAndHold = true
                                        val clickEvent = JSONObject().apply {
                                            put("kind", "mouse_button")
                                            put("button", 0) // Left mouse button down
                                            put("pressed", true)
                                            put("x", virtualCursorX)
                                            put("y", virtualCursorY)
                                            put("viewport_w", width)
                                            put("viewport_h", height)
                                            put("display_id", 0)
                                        }
                                        client.sendInputEvent(clickEvent)
                                        isLeftMouseDown = true
                                    }
                                } else {
                                    // Direct touch: send mouse move + Left Click Down immediately
                                    val moveEvent = JSONObject().apply {
                                        put("kind", "mouse_move")
                                        put("x", normX)
                                        put("y", normY)
                                        put("viewport_w", width)
                                        put("viewport_h", height)
                                        put("display_id", 0)
                                    }
                                    client.sendInputEvent(moveEvent)
                                    
                                    val clickEvent = JSONObject().apply {
                                        put("kind", "mouse_button")
                                        put("button", 0) // Left click (0)
                                        put("pressed", true)
                                        put("x", normX)
                                        put("y", normY)
                                        put("viewport_w", width)
                                        put("viewport_h", height)
                                        put("display_id", 0)
                                    }
                                    client.sendInputEvent(clickEvent)
                                    isLeftMouseDown = true
                                    virtualCursorX = normX
                                    virtualCursorY = normY
                                }
                                change.consume()
                            } else if (change.pressed) {
                                if (isTrackpadMode) {
                                    val deltaX = change.position.x - change.previousPosition.x
                                    val deltaY = change.position.y - change.previousPosition.y
                                    
                                    val sensitivity = 1.8f
                                    val normDeltaX = (deltaX / width.toFloat()) * sensitivity
                                    val normDeltaY = (deltaY / height.toFloat()) * sensitivity
                                    
                                    virtualCursorX = (virtualCursorX + normDeltaX).coerceIn(0f, 1f)
                                    virtualCursorY = (virtualCursorY + normDeltaY).coerceIn(0f, 1f)

                                    val moveEvent = JSONObject().apply {
                                        put("kind", "mouse_move")
                                        put("x", virtualCursorX)
                                        put("y", virtualCursorY)
                                        put("viewport_w", width)
                                        put("viewport_h", height)
                                        put("display_id", 0)
                                    }
                                    client.sendInputEvent(moveEvent)
                                } else {
                                    // Direct Touch Move / Drag
                                    val moveEvent = JSONObject().apply {
                                        put("kind", "mouse_move")
                                        put("x", normX)
                                        put("y", normY)
                                        put("viewport_w", width)
                                        put("viewport_h", height)
                                        put("display_id", 0)
                                    }
                                    client.sendInputEvent(moveEvent)
                                    virtualCursorX = normX
                                    virtualCursorY = normY

                                    val duration = System.currentTimeMillis() - touchDownTime
                                    val dx = pos.x - touchDownPos.x
                                    val dy = pos.y - touchDownPos.y
                                    val distance = sqrt(dx * dx + dy * dy)
                                    if (!isLongPressTriggered && duration > 600 && distance < 20f) {
                                        isLongPressTriggered = true
                                        if (isLeftMouseDown) {
                                            val leftUp = JSONObject().apply {
                                                put("kind", "mouse_button")
                                                put("button", 0)
                                                put("pressed", false)
                                                put("x", normX)
                                                put("y", normY)
                                                put("viewport_w", width)
                                                put("viewport_h", height)
                                                put("display_id", 0)
                                            }
                                            client.sendInputEvent(leftUp)
                                            isLeftMouseDown = false
                                        }
                                        val rightDown = JSONObject().apply {
                                            put("kind", "mouse_button")
                                            put("button", 1) // Right click
                                            put("pressed", true)
                                            put("x", normX)
                                            put("y", normY)
                                            put("viewport_w", width)
                                            put("viewport_h", height)
                                            put("display_id", 0)
                                        }
                                        client.sendInputEvent(rightDown)
                                        isRightMouseDown = true
                                    }
                                }
                                change.consume()
                            }
                        } else if (numFingers == 2) {
                            isMultiTouch = true
                            val p1 = pressedPointers[0]
                            val p2 = pressedPointers[1]

                            if (p2.changedToDown() || p1.changedToDown()) {
                                twoFingerDownTime = System.currentTimeMillis()
                                twoFingerStartPos1 = p1.position
                                twoFingerStartPos2 = p2.position
                                twoFingerTapPossible = true
                                twoFingerScrollStarted = false
                            }

                            val dy1 = p1.position.y - p1.previousPosition.y
                            val dy2 = p2.position.y - p2.previousPosition.y
                            val avgDeltaY = (dy1 + dy2) / 2f

                            val d1x = p1.position.x - twoFingerStartPos1.x
                            val d1y = p1.position.y - twoFingerStartPos1.y
                            val dist1 = sqrt(d1x * d1x + d1y * d1y)

                            val d2x = p2.position.x - twoFingerStartPos2.x
                            val d2y = p2.position.y - twoFingerStartPos2.y
                            val dist2 = sqrt(d2x * d2x + d2y * d2y)

                            if (dist1 > 25f || dist2 > 25f) {
                                twoFingerTapPossible = false
                            }

                            if (abs(avgDeltaY) > 2f) {
                                twoFingerScrollStarted = true
                                val scrollDelta = -avgDeltaY / 80f
                                val scrollEvent = JSONObject().apply {
                                    put("kind", "mouse_scroll")
                                    put("delta_x", 0f)
                                    put("delta_y", scrollDelta)
                                }
                                client.sendInputEvent(scrollEvent)
                            }
                            
                            p1.consume()
                            p2.consume()
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

                    // Touch mode indicator/toggle
                    Button(
                        onClick = { isTrackpadMode = !isTrackpadMode },
                        enabled = true,
                        colors = ButtonDefaults.buttonColors(
                            containerColor = if (isTrackpadMode) Color(0xFF6366F1) else Color(0xFF334155)
                        )
                    ) {
                        Text(if (isTrackpadMode) "🖱️ Virtual Mouse" else "📱 Direct Touch", color = Color.White, fontSize = 12.sp)
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

        // Virtual Cursor overlay (only in Trackpad/Virtual Mouse mode)
        if (isTrackpadMode) {
            Box(
                modifier = Modifier
                    .offset {
                        val pxX = (virtualCursorX * viewSize.width).toInt()
                        val pxY = (virtualCursorY * viewSize.height).toInt()
                        androidx.compose.ui.unit.IntOffset(pxX, pxY)
                    }
                    .size(24.dp)
            ) {
                androidx.compose.foundation.Canvas(modifier = Modifier.fillMaxSize()) {
                    val path = androidx.compose.ui.graphics.Path().apply {
                        moveTo(0f, 0f)
                        lineTo(15.dp.toPx(), 15.dp.toPx())
                        lineTo(9.dp.toPx(), 15.dp.toPx())
                        lineTo(14.dp.toPx(), 24.dp.toPx())
                        lineTo(11.dp.toPx(), 25.dp.toPx())
                        lineTo(6.dp.toPx(), 16.dp.toPx())
                        lineTo(2.dp.toPx(), 20.dp.toPx())
                        close()
                    }
                    drawPath(path, color = Color.White)
                    drawPath(path, color = Color.Black, style = androidx.compose.ui.graphics.drawscope.Stroke(width = 2f))
                }
            }
        }

        // Left & Right Click overlay buttons (only in Trackpad/Virtual Mouse mode)
        if (isTrackpadMode && connectionStatus == "Connected" && !showMenu) {
            Row(
                modifier = Modifier
                    .align(Alignment.BottomStart)
                    .padding(16.dp)
                    .background(Color(0x800F172A), RoundedCornerShape(12.dp))
                    .padding(8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                Button(
                    onClick = {
                        val client = activeClient ?: return@Button
                        val width = viewSize.width
                        val height = viewSize.height
                        val clickDown = JSONObject().apply {
                            put("kind", "mouse_button")
                            put("button", 0) // Left click
                            put("pressed", true)
                            put("x", virtualCursorX)
                            put("y", virtualCursorY)
                            put("viewport_w", width)
                            put("viewport_h", height)
                            put("display_id", 0)
                        }
                        val clickUp = JSONObject().apply {
                            put("kind", "mouse_button")
                            put("button", 0)
                            put("pressed", false)
                            put("x", virtualCursorX)
                            put("y", virtualCursorY)
                            put("viewport_w", width)
                            put("viewport_h", height)
                            put("display_id", 0)
                        }
                        client.sendInputEvent(clickDown)
                        client.sendInputEvent(clickUp)
                    },
                    colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF6366F1)),
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                    modifier = Modifier.height(38.dp)
                ) {
                    Text("L-Click", fontSize = 12.sp, fontWeight = FontWeight.Bold)
                }

                Button(
                    onClick = {
                        val client = activeClient ?: return@Button
                        val width = viewSize.width
                        val height = viewSize.height
                        val clickDown = JSONObject().apply {
                            put("kind", "mouse_button")
                            put("button", 1) // Right click
                            put("pressed", true)
                            put("x", virtualCursorX)
                            put("y", virtualCursorY)
                            put("viewport_w", width)
                            put("viewport_h", height)
                            put("display_id", 0)
                        }
                        val clickUp = JSONObject().apply {
                            put("kind", "mouse_button")
                            put("button", 1)
                            put("pressed", false)
                            put("x", virtualCursorX)
                            put("y", virtualCursorY)
                            put("viewport_w", width)
                            put("viewport_h", height)
                            put("display_id", 0)
                        }
                        client.sendInputEvent(clickDown)
                        client.sendInputEvent(clickUp)
                    },
                    colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF475569)),
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                    modifier = Modifier.height(38.dp)
                ) {
                    Text("R-Click", fontSize = 12.sp, fontWeight = FontWeight.Bold)
                }
            }
        }
    }
}
