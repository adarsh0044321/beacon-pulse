import asyncio
import json
import logging
import sys
import time
import secrets

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    handlers=[logging.StreamHandler(sys.stdout)]
)
logger = logging.getLogger("SignalingServer")

# Maps pairing_code -> {
#   "host": websocket,
#   "player": websocket_or_None,
#   "token": 128_bit_hex_token,
#   "created_at": float_timestamp,
#   "last_heartbeat": float_timestamp,
#   "used": bool
# }
sessions = {}

# Maps websocket -> (pairing_code, role)
connections = {}

async def cleanup_session(code, reason=""):
    if code in sessions:
        session = sessions.pop(code)
        logger.info(f"Cleaning up session {code}. Reason: {reason}")
        
        # Close host websocket
        host_ws = session.get("host")
        if host_ws:
            connections.pop(host_ws, None)
            try:
                await host_ws.close()
            except Exception:
                pass
                
        # Close host_proxy websocket
        host_proxy = session.get("host_proxy")
        if host_proxy:
            connections.pop(host_proxy, None)
            try:
                await host_proxy.close()
            except Exception:
                pass

        # Close player websocket
        player_ws = session.get("player")
        if player_ws:
            connections.pop(player_ws, None)
            try:
                await player_ws.send(json.dumps({"type": "peer_disconnected"}))
                await player_ws.close()
            except Exception:
                pass

async def session_timeout_loop():
    while True:
        await asyncio.sleep(5)
        now = time.time()
        expired_codes = []
        for code, session in list(sessions.items()):
            # 5-minute initial connection timeout if player hasn't joined
            if session["player"] is None and (now - session["created_at"]) > 300:
                expired_codes.append((code, "Pairing code expired (5 min)"))
            # 30-second heartbeat timeout
            elif (now - session["last_heartbeat"]) > 30:
                expired_codes.append((code, "Heartbeat timeout (30s)"))
                
        for code, reason in expired_codes:
            await cleanup_session(code, reason)

async def handler(websocket, path="/"):
    logger.info(f"New connection established from {websocket.remote_address}")
    try:
        async for message in websocket:
            is_signaling_json = False
            data = None
            try:
                data = json.loads(message)
                if isinstance(data, dict) and "type" in data:
                    is_signaling_json = True
            except (json.JSONDecodeError, TypeError):
                pass

            if not is_signaling_json:
                conn_info = connections.get(websocket)
                if conn_info:
                    code, role = conn_info
                    session = sessions.get(code)
                    if session:
                        peer_ws = None
                        if role == "player":
                            peer_ws = session.get("host_proxy")
                        elif role == "host_proxy":
                            peer_ws = session.get("player")
                        
                        if peer_ws:
                            try:
                                await peer_ws.send(message)
                            except Exception as e:
                                logger.error(f"Error proxying message from {role} to peer: {e}")
                        continue
                logger.warning(f"Discarding unroutable message: {message[:100]}")
                continue

            msg_type = data.get("type")

            logger.info(f"Received message type '{msg_type}' from {websocket.remote_address}")

            if msg_type == "register_host":
                code = data.get("pairing_code")
                if not code:
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Missing pairing code"}))
                    continue

                if code in sessions:
                    # Clean up old session if active but same code is re-registered
                    logger.warning(f"Pairing code {code} already registered. Evicting old session.")
                    await cleanup_session(code, "Re-registration")

                token = secrets.token_hex(16) # 128-bit secure session token
                now = time.time()
                sessions[code] = {
                    "host": websocket,
                    "player": None,
                    "token": token,
                    "created_at": now,
                    "last_heartbeat": now,
                    "used": False
                }
                connections[websocket] = (code, "host")
                logger.info(f"Host successfully registered pairing code: {code}")
                await websocket.send(json.dumps({
                    "type": "registration_success",
                    "role": "host",
                    "session_token": token
                }))

            elif msg_type == "register_player":
                code = data.get("pairing_code")
                if not code:
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Missing pairing code"}))
                    continue

                if code not in sessions:
                    logger.warning(f"Player tried to register for inactive pairing code: {code}")
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Pairing code not found"}))
                    continue

                session = sessions[code]
                if session["used"]:
                    logger.warning(f"Player tried to register for already used code: {code}")
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Pairing code already used"}))
                    continue

                if time.time() - session["created_at"] > 300:
                    logger.warning(f"Player tried to register for expired code: {code}")
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Pairing code expired"}))
                    continue

                if session["player"] is not None:
                    logger.warning(f"Player already connected to pairing code: {code}")
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Host session full"}))
                    continue

                session["player"] = websocket
                connections[websocket] = (code, "player")
                logger.info(f"Player successfully registered for pairing code: {code}")
                await websocket.send(json.dumps({"type": "registration_success", "role": "player"}))

            elif msg_type == "offer":
                # Forward SDP Offer and Candidates from Player to Host
                code = connections.get(websocket, (None, None))[0]
                if not code or code not in sessions:
                    continue
                session = sessions[code]
                host_ws = session["host"]
                if host_ws:
                    logger.info(f"Forwarding SDP Offer for session {code} from player to host")
                    await host_ws.send(json.dumps({
                        "type": "offer",
                        "session_token": session["token"],
                        "sdp": data.get("sdp"),
                        "candidates": data.get("candidates", [])
                    }))

            elif msg_type == "answer":
                # Forward SDP Answer and Candidates from Host to Player (Validate Token)
                token = data.get("session_token")
                session_found = None
                code_found = None
                for code, s in sessions.items():
                    if s["token"] == token:
                        session_found = s
                        code_found = code
                        break

                if not session_found:
                    logger.warning(f"No session found for token: {token}")
                    continue

                player_ws = session_found["player"]
                if player_ws:
                    logger.info(f"Forwarding SDP Answer for session {code_found} from host to player")
                    await player_ws.send(json.dumps({
                        "type": "answer",
                        "sdp": data.get("sdp"),
                        "candidates": data.get("candidates", [])
                    }))
                    # Save the answer connection as host_proxy
                    connections[websocket] = (code_found, "host_proxy")
                    session_found["host_proxy"] = websocket
                    # Mark session as used (single-use constraint)
                    session_found["used"] = True

            elif msg_type == "heartbeat":
                # Handle host heartbeat to prevent inactivity timeout
                code = connections.get(websocket, (None, None))[0]
                if not code or code not in sessions:
                    continue
                session = sessions[code]
                
                token = data.get("session_token")
                if token == session["token"]:
                    session["last_heartbeat"] = time.time()
                    debug_msg = f"Heartbeat received for session {code}"
                    # logger.debug(debug_msg)

    except Exception as e:
        logger.error(f"Connection error: {e}")
    finally:
        # Cleanup connection
        if websocket in connections:
            code, role = connections.pop(websocket)
            logger.info(f"Connection closed for {websocket.remote_address} (role={role}, code={code})")
            if code in sessions:
                session = sessions[code]
                if role == "host":
                    # Host disconnected -> close session completely
                    await cleanup_session(code, "Host socket closed")
                elif role == "host_proxy":
                    session["host_proxy"] = None
                    await cleanup_session(code, "Host proxy socket closed")
                elif role == "player":
                    # Player disconnected -> inform host, allow reconnect or close if used
                    session["player"] = None
                    host_ws = session["host"]
                    if host_ws:
                        try:
                            await host_ws.send(json.dumps({"type": "peer_disconnected"}))
                        except Exception:
                            pass
                    if session["used"]:
                        # If session was already established and player left, clean up
                        await cleanup_session(code, "Player disconnected after session established")

async def main():
    import argparse
    parser = argparse.ArgumentParser(description="Beacon-Pulse Signaling Server")
    parser.add_argument("--port", type=int, default=8080, help="Port to bind (default: 8080)")
    parser.add_argument("--host", type=str, default="0.0.0.0", help="Host address to bind (default: 0.0.0.0)")
    args = parser.parse_args()

    try:
        import websockets
    except ImportError:
        logger.error("The 'websockets' library is required to run the signaling server.")
        logger.error("Please install it using: pip install websockets")
        sys.exit(1)

    asyncio.create_task(session_timeout_loop())

    async with websockets.serve(handler, args.host, args.port):
        logger.info(f"Signaling server running on ws://{args.host}:{args.port}")
        await asyncio.Future()  # run forever

if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        logger.info("Signaling server stopped")
