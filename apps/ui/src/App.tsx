import React, { useState, useEffect } from 'react';
import { listen } from './store/ipc';
import { Home } from './pages/Home';
import { Host } from './pages/Host';
import { Client } from './pages/Client';
import { Settings } from './pages/Settings';
import { useSessionStore } from './store/sessionStore';
import { useSettingsStore } from './store/settingsStore';
import { useToastStore } from './store/toastStore';
import './App.css';

export type Page = 'home' | 'host' | 'client' | 'settings';

// ─────────────────────────────────────────────────────────────────────────────
// Global IPC event listener
// Listens for service-pushed events that should update global Zustand state.
// ─────────────────────────────────────────────────────────────────────────────

function useGlobalIpcEvents() {
  const { setEncoderInfo, updateStats, setShareActive, addConnectedClient, removeConnectedClient } = useSessionStore();
  const { addToast } = useToastStore();

  useEffect(() => {
    // encoder_ready — service pushes this right after hardware encoder starts
    const unlistenEncoder = listen<{
      encoder_name: string;
      vendor: string;
      hw_accelerated: boolean;
    }>('encoder_ready', (event) => {
      setEncoderInfo(event.payload);
      addToast('Encoder Initialized', `${event.payload.encoder_name} started successfully.`, 'success');
    });

    // stats — host session emits Stats every ~500 ms
    const unlistenStats = listen<{
      fps: number;
      encode_ms: number;
      latency_ms: number;
      bitrate_kbps: number;
      client_count: number;
      gpu_path_active: boolean;
    }>('stats', (event) => {
      updateStats({
        fps:             event.payload.fps,
        encode_ms:       event.payload.encode_ms ?? 0,
        latency_ms:      event.payload.latency_ms,
        bitrate_kbps:    event.payload.bitrate_kbps,
        client_count:    event.payload.client_count,
        gpu_path_active: event.payload.gpu_path_active ?? false,
      });
    });

    // share_started — service confirms stream is live (use pure state setter, not invoke)
    const unlistenStarted = listen<{ hwnd?: number; target?: any }>('share_started', (event) => {
      setShareActive(true, event.payload.target || event.payload.hwnd);
      addToast('Sharing Started', 'Screen stream is now broadcasting on the local network.', 'success');
    });

    // share_stopped — service confirms stream has ended (use pure state setter, not invoke)
    const unlistenStopped = listen('share_stopped', () => {
      setShareActive(false);
      addToast('Sharing Stopped', 'Screen stream has ended.', 'info');
    });

    // client_connected — service notifies when a new client connects
    const unlistenClientConnected = listen<{ client_id: string; display_name: string; addr: string }>('client_connected', (event) => {
      addConnectedClient({
        client_id: event.payload.client_id,
        display_name: event.payload.display_name,
        addr: event.payload.addr,
        permissions: { input_control: true, clipboard: true, audio: true },
        stats: { fps: 60, latency_ms: 0, bitrate_kbps: 0 }
      });
      addToast('Client Connected', `${event.payload.display_name} connected to stream.`, 'success');
    });

    // client_disconnected — service notifies when a client leaves
    const unlistenClientDisconnected = listen<{ client_id: string }>('client_disconnected', (event) => {
      const state = useSessionStore.getState();
      const client = state.connectedClients.find(c => c.client_id === event.payload.client_id);
      removeConnectedClient(event.payload.client_id);
      if (client) {
        addToast('Client Disconnected', `${client.display_name} left the stream.`, 'info');
      }
    });

    return () => {
      unlistenEncoder.then(f => f());
      unlistenStats.then(f => f());
      unlistenStarted.then(f => f());
      unlistenStopped.then(f => f());
      unlistenClientConnected.then(f => f());
      unlistenClientDisconnected.then(f => f());
    };
  }, [setEncoderInfo, updateStats, setShareActive, addConnectedClient, removeConnectedClient, addToast]);
}

export const App: React.FC = () => {
  const mode = import.meta.env.MODE;
  const loadSettings = useSettingsStore(state => state.load);
  const { toasts, removeToast } = useToastStore();

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  const getInitialPage = (): Page => {
    if (mode === 'host') return 'host';
    if (mode === 'player') return 'client';
    return 'home';
  };

  const [page, setPage] = useState<Page>(getInitialPage());
  useGlobalIpcEvents();

  const navigate = (p: Page) => {
    if (mode === 'host' && p !== 'host' && p !== 'settings') {
      return;
    }
    if (mode === 'player' && p !== 'client' && p !== 'settings') {
      return;
    }
    setPage(p);
  };

  return (
    <div className="app">
      {/* Premium background mesh and float blobs */}
      <div className="bg-gradient-container">
        <div className="bg-orb orb-blue" />
        <div className="bg-orb orb-purple" />
        <div className="bg-orb orb-emerald" />
      </div>

      {page === 'home'     && <Home     onNavigate={navigate} />}
      {page === 'host'     && <Host     onNavigate={navigate} />}
      {page === 'client'   && <Client   onNavigate={navigate} />}
      {page === 'settings' && <Settings onNavigate={navigate} />}

      {/* Global Glass Toasts overlay */}
      <div className="toast-container">
        {toasts.map(toast => (
          <div key={toast.id} className={`toast-item ${toast.type}`}>
            <div className="toast-body">
              <div className="toast-title">{toast.title}</div>
              <div className="toast-message">{toast.message}</div>
            </div>
            <button className="toast-close" onClick={() => removeToast(toast.id)}>✕</button>
          </div>
        ))}
      </div>
    </div>
  );
};

export default App;
