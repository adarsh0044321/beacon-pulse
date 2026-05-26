import React, { useState, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Home } from './pages/Home';
import { Host } from './pages/Host';
import { Client } from './pages/Client';
import { Settings } from './pages/Settings';
import { useSessionStore } from './store/sessionStore';
import './App.css';

export type Page = 'home' | 'host' | 'client' | 'settings';

// ─────────────────────────────────────────────────────────────────────────────
// Global IPC event listener
// Listens for service-pushed events that should update global Zustand state.
// ─────────────────────────────────────────────────────────────────────────────

function useGlobalIpcEvents() {
  const { setEncoderInfo, updateStats, setShareActive } = useSessionStore();

  useEffect(() => {
    // encoder_ready — service pushes this right after hardware encoder starts
    const unlistenEncoder = listen<{
      encoder_name: string;
      vendor: string;
      hw_accelerated: boolean;
    }>('encoder_ready', (event) => {
      setEncoderInfo(event.payload);
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
    const unlistenStarted = listen<{ hwnd: number }>('share_started', (event) => {
      setShareActive(true, event.payload.hwnd);
    });

    // share_stopped — service confirms stream has ended (use pure state setter, not invoke)
    const unlistenStopped = listen('share_stopped', () => {
      setShareActive(false);
    });

    return () => {
      unlistenEncoder.then(f => f());
      unlistenStats.then(f => f());
      unlistenStarted.then(f => f());
      unlistenStopped.then(f => f());
    };
  }, [setEncoderInfo, updateStats, setShareActive]);
}

export const App: React.FC = () => {
  const mode = import.meta.env.MODE;

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
      {page === 'home'     && <Home     onNavigate={navigate} />}
      {page === 'host'     && <Host     onNavigate={navigate} />}
      {page === 'client'   && <Client   onNavigate={navigate} />}
      {page === 'settings' && <Settings onNavigate={navigate} />}
    </div>
  );
};

export default App;
