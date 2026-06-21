package com.beaconpulse.player.ui

import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import org.json.JSONObject
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.SocketTimeoutException

data class DiscoveredHost(
    val name: String,
    val ip: String,
    val port: Int,
    val monitorCount: Int,
    val quality: String
)

data class RecentConnection(
    val ip: String,
    val port: Int,
    val code: String?
)

private fun loadRecentConnections(sharedPrefs: android.content.SharedPreferences): List<RecentConnection> {
    val list = mutableListOf<RecentConnection>()
    val jsonStr = sharedPrefs.getString("recent_connections_json", null) ?: return list
    try {
        val array = org.json.JSONArray(jsonStr)
        for (i in 0 until array.length()) {
            val obj = array.getJSONObject(i)
            list.add(
                RecentConnection(
                    ip = obj.getString("ip"),
                    port = obj.getInt("port"),
                    code = if (obj.isNull("code")) null else obj.getString("code")
                )
            )
        }
    } catch (e: Exception) {
        e.printStackTrace()
    }
    return list
}

private fun saveRecentConnection(
    sharedPrefs: android.content.SharedPreferences,
    ip: String,
    port: Int,
    code: String?
) {
    val current = loadRecentConnections(sharedPrefs).toMutableList()
    current.removeAll { it.ip == ip && it.port == port }
    current.add(0, RecentConnection(ip, port, code))
    val trimmed = current.take(5)
    
    val array = org.json.JSONArray()
    for (item in trimmed) {
        val obj = org.json.JSONObject().apply {
            put("ip", item.ip)
            put("port", item.port)
            put("code", item.code ?: org.json.JSONObject.NULL)
        }
        array.put(obj)
    }
    sharedPrefs.edit().putString("recent_connections_json", array.toString()).apply()
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DashboardScreen(
    onConnect: (List<String>, Int, String?) -> Unit
) {
    val context = LocalContext.current
    val sharedPrefs = remember { context.getSharedPreferences("beacon_pulse_prefs", android.content.Context.MODE_PRIVATE) }
    
    var manualIp by remember { mutableStateOf(sharedPrefs.getString("last_manual_ip", "") ?: "") }
    var manualPort by remember { mutableStateOf(sharedPrefs.getString("last_manual_port", "45101") ?: "45101") }
    var manualCode by remember { mutableStateOf(sharedPrefs.getString("last_manual_code", "") ?: "") }

    val recentConnections = remember { mutableStateListOf<RecentConnection>() }

    LaunchedEffect(Unit) {
        recentConnections.clear()
        recentConnections.addAll(loadRecentConnections(sharedPrefs))
    }

    // State list for dynamically discovered hosts
    val discoveredHosts = remember { mutableStateListOf<DiscoveredHost>() }

    // Register ZXing barcode scan launcher
    val scanLauncher = rememberLauncherForActivityResult(
        contract = ScanContract(),
        onResult = { result ->
            val rawValue = result.contents
            if (rawValue != null) {
                try {
                    val json = JSONObject(rawValue)
                    val ipsArray = json.getJSONArray("ips")
                    val ipsList = mutableListOf<String>()
                    for (i in 0 until ipsArray.length()) {
                        ipsList.add(ipsArray.getString(i))
                    }
                    val port = json.optInt("port", 45101)
                    val code = json.optString("code", "")
                    val pairingCode = if (code.isEmpty()) null else code
                    
                    if (ipsList.isNotEmpty()) {
                        val primaryIp = ipsList[0]
                        manualIp = primaryIp
                        manualPort = port.toString()
                        manualCode = code

                        sharedPrefs.edit().apply {
                            putString("last_manual_ip", primaryIp)
                            putString("last_manual_port", port.toString())
                            putString("last_manual_code", code)
                            apply()
                        }
                        saveRecentConnection(sharedPrefs, primaryIp, port, pairingCode)
                    }
                    
                    onConnect(ipsList, port, pairingCode)
                } catch (e: Exception) {
                    Toast.makeText(context, "Invalid QR payload. Raw: $rawValue", Toast.LENGTH_LONG).show()
                }
            }
        }
    )

    // Start background UDP broadcast discovery listener
    DisposableEffect(Unit) {
        var isScanning = true
        var socket: DatagramSocket? = null
        val scanThread = Thread {
            try {
                // Bind to port 45199 with reuseAddress = true
                socket = DatagramSocket(45199).apply {
                    reuseAddress = true
                    soTimeout = 1000
                }
                val buffer = ByteArray(1024)
                val packet = DatagramPacket(buffer, buffer.size)

                while (isScanning) {
                    try {
                        socket?.receive(packet)
                        val dataStr = String(buffer, 0, packet.length, charset("UTF-8"))
                        val json = JSONObject(dataStr)
                        if (json.optInt("magic") == 1279340115) { // MAGIC "LANS"
                            val name = json.optString("name", "Unknown Host")
                            val port = json.optInt("port", 45101)
                            val ip = packet.address.hostAddress ?: continue
                            
                            val host = DiscoveredHost(
                                name = name,
                                ip = ip,
                                port = port,
                                monitorCount = 1,
                                quality = "Excellent (LAN)"
                            )
                            
                            // Update UI state list
                            if (discoveredHosts.none { it.ip == ip && it.port == port }) {
                                discoveredHosts.add(host)
                            }
                        }
                    } catch (e: SocketTimeoutException) {
                        // Timeout, loop again
                    } catch (e: Exception) {
                        // ignore packet error
                    }
                }
            } catch (e: Exception) {
                e.printStackTrace()
            } finally {
                try {
                    socket?.close()
                } catch (e: Exception) {}
            }
        }
        scanThread.start()

        onDispose {
            isScanning = false
            try {
                socket?.close()
            } catch (e: Exception) {}
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(Color(0xFF0F172A)) // Sleek dark slate
            .padding(16.dp)
    ) {
        // App Header
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                text = "📡 Beacon-Pulse",
                color = Color.White,
                fontSize = 24.sp,
                fontWeight = FontWeight.Bold
            )
            IconButton(onClick = { 
                // Clear host list to force rediscovery
                discoveredHosts.clear()
                Toast.makeText(context, "Scanning for hosts...", Toast.LENGTH_SHORT).show()
            }) {
                Text("🔄", fontSize = 20.sp)
            }
        }

        Spacer(modifier = Modifier.height(24.dp))

        // Discovered Hosts Section
        Text(
            text = "Discovered Hosts (LAN)",
            color = Color(0xFF94A3B8),
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold
        )
        Spacer(modifier = Modifier.height(8.dp))

        if (discoveredHosts.isEmpty()) {
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(Color(0xFF1E293B)),
                contentAlignment = Alignment.Center
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    CircularProgressIndicator(color = Color(0xFF6366F1))
                    Spacer(modifier = Modifier.height(12.dp))
                    Text(
                        text = "Listening for host broadcasts...",
                        color = Color(0xFF94A3B8),
                        fontSize = 12.sp
                    )
                }
            }
        } else {
            LazyColumn(
                verticalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier.weight(1f)
            ) {
                items(discoveredHosts) { host ->
                    HostCard(host = host, onClick = { 
                        manualIp = host.ip
                        manualPort = host.port.toString()
                        manualCode = ""
                        sharedPrefs.edit().apply {
                            putString("last_manual_ip", host.ip)
                            putString("last_manual_port", host.port.toString())
                            putString("last_manual_code", "")
                            apply()
                        }
                        saveRecentConnection(sharedPrefs, host.ip, host.port, null)
                        onConnect(listOf(host.ip), host.port, null) 
                    })
                }
            }
        }

        Spacer(modifier = Modifier.height(16.dp))

        // Manual Connection Settings
        Card(
            colors = CardDefaults.cardColors(containerColor = Color(0xFF1E293B)),
            shape = RoundedCornerShape(12.dp),
            modifier = Modifier.fillMaxWidth()
        ) {
            Column(modifier = Modifier.padding(16.dp)) {
                Text(
                    text = "Manual Connection",
                    color = Color.White,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Bold
                )
                
                if (recentConnections.isNotEmpty()) {
                    Spacer(modifier = Modifier.height(8.dp))
                    Text(
                        text = "Recent Connections",
                        color = Color(0xFF94A3B8),
                        fontSize = 11.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                    Spacer(modifier = Modifier.height(6.dp))
                    LazyRow(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        items(recentConnections) { recent ->
                            Box(
                                modifier = Modifier
                                    .clip(RoundedCornerShape(16.dp))
                                    .background(Color(0xFF334155))
                                    .clickable {
                                        manualIp = recent.ip
                                        manualPort = recent.port.toString()
                                        manualCode = recent.code ?: ""
                                    }
                                    .padding(horizontal = 12.dp, vertical = 6.dp)
                            ) {
                                Text(
                                    text = "${recent.ip}:${recent.port}",
                                    color = Color.White,
                                    fontSize = 12.sp,
                                    fontWeight = FontWeight.Medium
                                )
                            }
                        }
                    }
                }

                Spacer(modifier = Modifier.height(12.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    OutlinedTextField(
                        value = manualIp,
                        onValueChange = { manualIp = it },
                        placeholder = { Text("IP Address", color = Color.Gray) },
                        colors = OutlinedTextFieldDefaults.colors(
                            focusedTextColor = Color.White,
                            unfocusedTextColor = Color.White,
                            focusedBorderColor = Color(0xFF6366F1),
                            unfocusedBorderColor = Color.DarkGray
                        ),
                        modifier = Modifier.weight(2f)
                    )

                    OutlinedTextField(
                        value = manualPort,
                        onValueChange = { manualPort = it },
                        placeholder = { Text("Port", color = Color.Gray) },
                        colors = OutlinedTextFieldDefaults.colors(
                            focusedTextColor = Color.White,
                            unfocusedTextColor = Color.White,
                            focusedBorderColor = Color(0xFF6366F1),
                            unfocusedBorderColor = Color.DarkGray
                        ),
                        modifier = Modifier.weight(1f)
                    )
                }
                
                Spacer(modifier = Modifier.height(8.dp))

                OutlinedTextField(
                    value = manualCode,
                    onValueChange = { manualCode = it },
                    placeholder = { Text("Optional Pairing Code (6 digits)", color = Color.Gray) },
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedTextColor = Color.White,
                        unfocusedTextColor = Color.White,
                        focusedBorderColor = Color(0xFF6366F1),
                        unfocusedBorderColor = Color.DarkGray
                    ),
                    modifier = Modifier.fillMaxWidth()
                )

                Spacer(modifier = Modifier.height(12.dp))

                Button(
                    onClick = {
                        if (manualIp.isNotBlank()) {
                            val ipTrimmed = manualIp.trim()
                            val port = manualPort.toIntOrNull() ?: 45101
                            val code = if (manualCode.isBlank()) null else manualCode
                            sharedPrefs.edit().apply {
                                putString("last_manual_ip", ipTrimmed)
                                putString("last_manual_port", port.toString())
                                putString("last_manual_code", code ?: "")
                                apply()
                            }
                            saveRecentConnection(sharedPrefs, ipTrimmed, port, code)
                            onConnect(listOf(ipTrimmed), port, code)
                        } else {
                            Toast.makeText(context, "Please enter host IP address", Toast.LENGTH_SHORT).show()
                        }
                    },
                    colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF6366F1)),
                    shape = RoundedCornerShape(8.dp),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("Connect", color = Color.White, fontWeight = FontWeight.Bold)
                }
            }
        }

        Spacer(modifier = Modifier.height(16.dp))

        // QR Code Pairing Button
        OutlinedButton(
            onClick = {
                val options = ScanOptions().apply {
                    setPrompt("Scan PC pairing QR code")
                    setBeepEnabled(false)
                    setOrientationLocked(true)
                }
                scanLauncher.launch(options)
            },
            colors = ButtonDefaults.outlinedButtonColors(contentColor = Color(0xFF38BDF8)),
            border = ButtonDefaults.outlinedButtonBorder.copy(width = 1.dp),
            shape = RoundedCornerShape(8.dp),
            modifier = Modifier
                .fillMaxWidth()
                .height(48.dp)
        ) {
            Text("📸 Scan QR Code", fontWeight = FontWeight.Bold)
        }
    }
}

@Composable
fun HostCard(
    host: DiscoveredHost,
    onClick: () -> Unit
) {
    Card(
        colors = CardDefaults.cardColors(containerColor = Color(0xFF1E293B)),
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .clickable { onClick() }
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Column {
                Text(
                    text = "🖥️ " + host.name,
                    color = Color.White,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Bold
                )
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    text = "IP: ${host.ip}  |  Port: ${host.port}",
                    color = Color(0xFF94A3B8),
                    fontSize = 12.sp
                )
                Text(
                    text = "Quality: ${host.quality}",
                    color = Color(0xFF22C55E),
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Medium
                )
            }
            Button(
                onClick = onClick,
                colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF6366F1)),
                shape = RoundedCornerShape(8.dp)
            ) {
                Text("Connect", color = Color.White, fontSize = 12.sp)
            }
        }
    }
}
