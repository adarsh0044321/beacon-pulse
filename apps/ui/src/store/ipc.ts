import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen } from '@tauri-apps/api/event';

const isTauri = typeof window !== 'undefined' && (
  (window as any).__TAURI__ !== undefined || 
  (window as any).__TAURI_INTERNALS__ !== undefined
);

let ws: WebSocket | null = null;
const wsMessageQueue: string[] = [];
const eventListeners = new Map<string, Set<(event: { payload: any }) => void>>();

interface PendingRequest {
  resolve: (val: any) => void;
  reject: (err: any) => void;
  cmd: string;
}
const requestQueue: PendingRequest[] = [];

function initWebSocket() {
  if (isTauri) return;
  if (ws) return;

  const isPlayer = import.meta.env.MODE === 'player';
  const defaultPort = isPlayer ? 45200 : 45199;

  let wsUrl = '';
  if (window.location.hostname && window.location.port && window.location.port !== '5173' && window.location.port !== '3000') {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    wsUrl = `${proto}//${window.location.host}`;
  } else {
    wsUrl = `ws://localhost:${defaultPort}`;
  }

  console.log(`[IPC] Connecting to backend WebSocket at ${wsUrl}`);
  const socket = new WebSocket(wsUrl);
  ws = socket;

  socket.onopen = () => {
    console.log('[IPC] Backend WebSocket connected');
    while (wsMessageQueue.length > 0) {
      const msg = wsMessageQueue.shift();
      if (msg) socket.send(msg);
    }
  };

  socket.onmessage = (event) => {
    try {
      const msg = JSON.parse(event.data);
      if (msg.type === 'response') {
        resolveNextPending(msg.data);
      } else if (msg.type === 'event') {
        const evData = msg.data;
        const evName = evData.event;
        if (evName) {
          const listeners = eventListeners.get(evName);
          if (listeners) {
            listeners.forEach(cb => {
              try {
                cb({ payload: evData });
              } catch (err) {
                console.error(`[IPC] Error in event listener for ${evName}:`, err);
              }
            });
          }
        }
      }
    } catch (e) {
      console.error('[IPC] Failed to parse WebSocket message:', e);
    }
  };

  socket.onclose = () => {
    console.warn('[IPC] Backend WebSocket disconnected. Reconnecting in 1s...');
    ws = null;
    setTimeout(initWebSocket, 1000);
  };

  socket.onerror = (err) => {
    console.error('[IPC] WebSocket error:', err);
  };
}

function resolveNextPending(responseData: any) {
  if (requestQueue.length === 0) return;

  let matchIndex = 0;
  if (responseData.windows) {
    matchIndex = requestQueue.findIndex(r => r.cmd === 'list_windows');
  } else if (responseData.monitors) {
    matchIndex = requestQueue.findIndex(r => r.cmd === 'list_monitors');
  } else if (responseData.hosts) {
    matchIndex = requestQueue.findIndex(r => r.cmd === 'discover_hosts');
  } else if (responseData.clients) {
    matchIndex = requestQueue.findIndex(r => r.cmd === 'get_active_clients');
  }

  if (matchIndex === -1) {
    matchIndex = 0;
  }

  const req = requestQueue.splice(matchIndex, 1)[0];
  if (req) {
    if (responseData.event === 'error') {
      req.reject(new Error(responseData.message || 'Unknown backend error'));
    } else {
      if (req.cmd === 'list_windows') {
        req.resolve(responseData.windows || []);
      } else if (req.cmd === 'list_monitors') {
        req.resolve(responseData.monitors || []);
      } else if (req.cmd === 'discover_hosts') {
        req.resolve(responseData.hosts || []);
      } else if (req.cmd === 'get_active_clients') {
        req.resolve(responseData.clients || []);
      } else {
        req.resolve(responseData);
      }
    }
  }
}

// Initialise
initWebSocket();

export async function invoke<T>(cmd: string, args?: any): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(cmd, args);
  }

  if (cmd === 'save_settings') {
    localStorage.setItem('lanshare_settings', JSON.stringify(args.settings));
    return Promise.resolve({} as T);
  }
  if (cmd === 'load_settings') {
    const s = localStorage.getItem('lanshare_settings');
    return Promise.resolve(s ? JSON.parse(s) : {}) as Promise<T>;
  }
  if (cmd === 'read_recent_logs') {
    return Promise.resolve(["Logs are only available in desktop standalone mode."] as any);
  }
  if (cmd === 'send_wol_packet') {
    return Promise.resolve({} as T);
  }

  let payload: any = null;

  switch (cmd) {
    case 'list_windows':
      payload = { cmd: 'list_windows' };
      break;
    case 'list_monitors':
      payload = { cmd: 'list_monitors' };
      break;
    case 'start_share':
      payload = { cmd: 'start_share', target: args.target };
      break;
    case 'stop_share':
      payload = { cmd: 'stop_share' };
      break;
    case 'set_bitrate':
      payload = { cmd: 'set_bitrate', kbps: args.kbps };
      break;
    case 'generate_pairing_code':
      payload = { cmd: 'generate_pairing_code' };
      break;
    case 'kick_client':
      payload = { cmd: 'kick_client', client_id: args.clientId };
      break;
    case 'get_active_clients':
      payload = { cmd: 'get_active_clients' };
      break;
    case 'discover_hosts':
      payload = { cmd: 'discover_hosts' };
      break;
    case 'connect_to_host':
      payload = {
        cmd: 'join_stream',
        host_ip: args.address,
        stream_port: args.port,
        recv_port: 45102,
        pairing_code: args.code || null,
        tls: args.tls || false
      };
      break;
    case 'disconnect_from_host':
      payload = { cmd: 'leave_stream' };
      break;
    case 'request_keyframe':
      payload = { cmd: 'request_keyframe' };
      break;
    case 'send_input':
      payload = { cmd: 'send_input', event: args.event };
      break;
    case 'send_file_start':
      payload = { cmd: 'send_file_start', name: args.name, size: args.size };
      break;
    case 'send_file_chunk':
      payload = { cmd: 'send_file_chunk', data: args.data };
      break;
    case 'send_file_end':
      payload = { cmd: 'send_file_end' };
      break;
    case 'list_host_dir':
      payload = { cmd: 'list_host_directory', path: args.path };
      break;
    case 'download_host_file':
      payload = { cmd: 'download_host_file', path: args.path };
      break;
    case 'host_file_action':
      payload = { cmd: 'host_file_action', action: args.action, path: args.path, new_path: args.newPath || null };
      break;
    case 'update_stream_settings':
      payload = { cmd: 'update_stream_settings', fps: args.fps || null, scale: args.scale || null, bitrate_bps: args.bitrateBps || null };
      break;
    default:
      return Promise.reject(new Error(`Unknown command: ${cmd}`));
  }

  return new Promise<T>((resolve, reject) => {
    requestQueue.push({ resolve, reject, cmd });
    const msgStr = JSON.stringify(payload);
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(msgStr);
    } else {
      wsMessageQueue.push(msgStr);
      initWebSocket();
    }
  });
}

export function listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<() => void> {
  if (isTauri) {
    return tauriListen<T>(event, handler);
  }

  let listeners = eventListeners.get(event);
  if (!listeners) {
    listeners = new Set();
    eventListeners.set(event, listeners);
  }
  listeners.add(handler);

  const unlisten = () => {
    const l = eventListeners.get(event);
    if (l) {
      l.delete(handler);
    }
  };
  return Promise.resolve(unlisten);
}
