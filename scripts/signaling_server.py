import asyncio
import json
import logging
import sys

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    handlers=[logging.StreamHandler(sys.stdout)]
)
logger = logging.getLogger("SignalingServer")

# Maps pairing_code -> (host_websocket, player_websocket)
sessions = {}

# Maps websocket -> pairing_code
connections = {}

async def handler(websocket, path="/"):
    logger.info(f"New connection established from {websocket.remote_address}")
    try:
        async for message in websocket:
            try:
                data = json.loads(message)
            except json.JSONDecodeError:
                logger.warning(f"Received non-JSON message: {message}")
                continue

            msg_type = data.get("type")
            if not msg_type:
                logger.warning(f"Message missing type: {data}")
                continue

            logger.info(f"Received message type '{msg_type}' from {websocket.remote_address}")

            if msg_type == "register_host":
                code = data.get("pairing_code")
                if not code:
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Missing pairing code"}))
                    continue

                if code in sessions:
                    logger.warning(f"Pairing code {code} already registered by another host")
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Pairing code in use"}))
                    continue

                sessions[code] = {"host": websocket, "player": None}
                connections[websocket] = (code, "host")
                logger.info(f"Host successfully registered pairing code: {code}")
                await websocket.send(json.dumps({"type": "registration_success", "role": "host"}))

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
                if session["player"] is not None:
                    logger.warning(f"Player already connected to pairing code: {code}")
                    await websocket.send(json.dumps({"type": "registration_failed", "reason": "Host session full"}))
                    continue

                session["player"] = websocket
                connections[websocket] = (code, "player")
                logger.info(f"Player successfully registered for pairing code: {code}")
                await websocket.send(json.dumps({"type": "registration_success", "role": "player"}))

            elif msg_type == "offer":
                # Host receives offer from player
                code = connections.get(websocket, (None, None))[0]
                if not code or code not in sessions:
                    continue
                session = sessions[code]
                host_ws = session["host"]
                if host_ws:
                    logger.info(f"Forwarding SDP Offer for session {code} from player to host")
                    await host_ws.send(json.dumps({
                        "type": "offer",
                        "sdp": data.get("sdp"),
                        "candidate_addr": data.get("candidate_addr")
                    }))

            elif msg_type == "answer":
                # Player receives answer from host
                code = connections.get(websocket, (None, None))[0]
                if not code or code not in sessions:
                    continue
                session = sessions[code]
                player_ws = session["player"]
                if player_ws:
                    logger.info(f"Forwarding SDP Answer for session {code} from host to player")
                    await player_ws.send(json.dumps({
                        "type": "answer",
                        "sdp": data.get("sdp"),
                        "candidate_addr": data.get("candidate_addr")
                    }))

    except Exception as e:
        logger.error(f"Connection error: {e}")
    finally:
        # Cleanup connection
        logger.info(f"Connection closed for {websocket.remote_address}")
        if websocket in connections:
            code, role = connections.pop(websocket)
            if code in sessions:
                session = sessions[code]
                if role == "host":
                    player_ws = session["player"]
                    if player_ws:
                        try:
                            await player_ws.send(json.dumps({"type": "peer_disconnected"}))
                        except Exception:
                            pass
                    sessions.pop(code, None)
                    logger.info(f"Host disconnected. Removed session for pairing code: {code}")
                elif role == "player":
                    session["player"] = None
                    host_ws = session["host"]
                    if host_ws:
                        try:
                            await host_ws.send(json.dumps({"type": "peer_disconnected"}))
                        except Exception:
                            pass
                    logger.info(f"Player disconnected from session for pairing code: {code}")

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

    async with websockets.serve(handler, args.host, args.port):
        logger.info(f"Signaling server running on ws://{args.host}:{args.port}")
        await asyncio.Future()  # run forever

if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        logger.info("Signaling server stopped")
