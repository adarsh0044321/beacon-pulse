import React, { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { ArrowLeft, Play, Square, RefreshCw, Users, Key, AlertTriangle, Cpu, Zap, Settings as SettingsIcon } from 'lucide-react';
import type { Page } from '../App';
import { useSessionStore, type WindowInfo } from '../store/sessionStore';

interface HostProps { onNavigate: (p: Page) => void; }

export const Host: React.FC<HostProps> = ({ onNavigate }) => {
  const {
    availableWindows, windowsLoading, fetchWindows,
    isSharing, activeHwnd, startShare, stopShare,
    pairingCode, pairingExpiresIn, generatePairingCode,
    connectedClients, kickClient,
    stats, updateStats,
    encoderInfo, setEncoderInfo,
  } = useSessionStore();

  const [selectedHwnd, setSelectedHwnd] = useState<number | null>(null);
  const [codeTimer, setCodeTimer] = useState(0);
  const [captureStatus, setCaptureStatus] = useState<{
    backend: string;
    isStale: boolean;
    renderSuspended: boolean;
    appKind: string;
    warning: string | null;
  }>({
    backend: 'WGC',
    isStale: false,
    renderSuspended: false,
    appKind: 'Unknown',
    warning: null,
  });

  const [loadError, setLoadError] = useState<string | null>(null);

  // Auto-retry window listing — the service may take 2–3 s to start via the watchdog.
  useEffect(() => {
    let cancelled = false;
    let retries = 0;
    const maxRetries = 6;
    const tryFetch = async () => {
      while (!cancelled && retries < maxRetries) {
        try {
          await fetchWindows();
          // If we got windows, stop retrying
          const state = useSessionStore.getState();
          if (state.availableWindows.length > 0) {
            setLoadError(null);
            return;
          }
        } catch {
          // Service might not be ready yet
        }
        retries++;
        if (retries < maxRetries) {
          setLoadError(`Connecting to service… (attempt ${retries}/${maxRetries})`);
          await new Promise(r => setTimeout(r, 1500));
        }
      }
      if (!cancelled) {
        setLoadError(null); // Clear the retrying message even if no windows found
      }
    };
    tryFetch();
    return () => { cancelled = true; };
  }, [fetchWindows]);

  // Listen for backend-switched events from the service.
  useEffect(() => {
    const unlisten = listen<{ to: string; reason: string }>('capture_backend_switched', ev => {
      setCaptureStatus(s => ({ ...s, backend: ev.payload.to }));
    });
    return () => { unlisten.then(f => f()); };
  }, []);

  // Listen for all service push events (Stats, PairingCode, EncoderReady, etc.)
  useEffect(() => {
    const unlisten = listen<string>('service-event', (event) => {
      try {
        const ev = typeof event.payload === 'string' 
          ? JSON.parse(event.payload) 
          : event.payload;
        switch (ev.event) {
          case 'stats':
            updateStats({
              fps: ev.fps ?? 0,
              encode_ms: ev.encode_ms ?? 0,
              latency_ms: ev.latency_ms ?? 0,
              bitrate_kbps: ev.bitrate_kbps ?? 0,
              client_count: ev.client_count ?? 0,
              gpu_path_active: ev.gpu_path_active ?? false,
            });
            break;
          case 'pairing_code':
            useSessionStore.setState({
              pairingCode: ev.code,
              pairingExpiresIn: ev.expires_in ?? 120,
            });
            break;
          case 'encoder_ready':
            setEncoderInfo({
              encoder_name: ev.encoder_name,
              vendor: ev.vendor,
              hw_accelerated: ev.hw_accelerated,
            });
            break;
          case 'client_connected':
            useSessionStore.getState().addConnectedClient({
              client_id: ev.client_id,
              display_name: ev.display_name,
              addr: ev.addr,
              permissions: {
                input_control: true,
                clipboard: true,
                audio: true,
              },
              stats: {
                fps: 60,
                latency_ms: 0,
                bitrate_kbps: 0,
              }
            });
            break;
          case 'client_disconnected':
            useSessionStore.getState().removeConnectedClient(ev.client_id);
            break;
        }
      } catch {
        // ignore parse errors
      }
    });
    return () => { unlisten.then(f => f()); };
  }, [updateStats, setEncoderInfo]);

  useEffect(() => {
    if (!pairingCode) return;
    setCodeTimer(pairingExpiresIn);
    const iv = setInterval(() => {
      setCodeTimer(t => {
        if (t <= 1) { clearInterval(iv); return 0; }
        return t - 1;
      });
    }, 1000);
    return () => clearInterval(iv);
  }, [pairingCode, pairingExpiresIn]);

  const handleStartShare = async () => {
    if (!selectedHwnd) return;
    // Find selected window info to show render-suspension warning early
    const win = availableWindows.find(w => w.hwnd === selectedHwnd);
    if (win?.suspends_render_when_minimized) {
      setCaptureStatus(s => ({
        ...s,
        appKind: win.app_kind ?? 'Unknown',
        warning: `${win.process_name} may pause rendering when minimized. LANShare will serve the last frame until rendering resumes.`,
      }));
    }
    await startShare(selectedHwnd);
    await generatePairingCode();
  };

  const getWindowIcon = (win: WindowInfo) => {
    const name = win.process_name.toLowerCase();
    if (name.includes('chrome')) return '🌐';
    if (name.includes('firefox')) return '🦊';
    if (name.includes('code')) return '💻';
    if (name.includes('notepad')) return '📝';
    if (name.includes('explorer')) return '📁';
    if (name.includes('discord')) return '💬';
    return '🖥️';
  };

  return (
    <div className="page">
      {/* Header */}
      <div className="page-header">
        {import.meta.env.MODE === 'host' ? (
          <button className="btn btn-ghost btn-sm" onClick={() => onNavigate('settings')}>
            <SettingsIcon size={14} /> Settings
          </button>
        ) : (
          <button className="btn btn-ghost btn-sm" onClick={() => onNavigate('home')}>
            <ArrowLeft size={14} /> Back
          </button>
        )}
        <h2 style={{ flex: 1 }}>Host Mode</h2>
        {isSharing && (
          <div className="badge badge-active">
            <div className="pulse-dot" /> Sharing Active
          </div>
        )}
      </div>

      <div className="page-content">
        {/* Window Picker */}
        {!isSharing && (
          <>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
              <h3>Select Window to Share</h3>
              <button className="btn btn-ghost btn-sm" onClick={fetchWindows} disabled={windowsLoading}>
                <RefreshCw size={12} className={windowsLoading ? 'spinner' : ''} />
                Refresh
              </button>
            </div>

            {windowsLoading ? (
              <div className="empty-state">
                <div className="spinner" />
                <p>{loadError || 'Scanning windows…'}</p>
              </div>
            ) : availableWindows.length === 0 ? (
              <div className="empty-state">
                <span className="empty-state-icon">🪟</span>
                <p>No visible windows found</p>
                <button className="btn btn-ghost btn-sm" onClick={fetchWindows} style={{ marginTop: '8px' }}>
                  <RefreshCw size={12} /> Try Again
                </button>
              </div>
            ) : (
              <div className="window-grid">
                {availableWindows.map(win => (
                  <div
                    key={win.hwnd}
                    id={`window-card-${win.hwnd}`}
                    className={`window-card ${selectedHwnd === win.hwnd ? 'selected' : ''}`}
                    onClick={() => setSelectedHwnd(win.hwnd)}
                  >
                    <div className="window-thumb">{getWindowIcon(win)}</div>
                    <div className="window-info">
                      <span className="window-title">{win.title}</span>
                      <span className="window-process">{win.process_name} · {win.width}×{win.height}</span>
                      <div style={{ display: 'flex', gap: '4px', flexWrap: 'wrap' }}>
                        {win.is_minimized && <span className="badge badge-warning" style={{ fontSize: '0.68rem', padding: '1px 6px' }}>Minimized</span>}
                        <span className="badge badge-inactive" style={{ fontSize: '0.68rem', padding: '1px 6px' }}>{win.app_kind}</span>
                        {win.suspends_render_when_minimized && (
                          <span title="This app may pause rendering when minimized"
                            style={{ fontSize: '0.7rem', color: 'var(--warning)', display: 'flex', alignItems: 'center', gap: '2px' }}>
                            ⚠
                          </span>
                        )}
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            )}

            <button
              id="btn-start-share"
              className="btn btn-success btn-lg btn-full"
              disabled={!selectedHwnd}
              onClick={handleStartShare}
            >
              <Play size={16} /> Start Sharing
            </button>
          </>
        )}

        {/* Active Share Panel */}
        {isSharing && (
          <>
            {/* Capture status warning */}
            {captureStatus.warning && (
              <div style={{
                display: 'flex', alignItems: 'flex-start', gap: '10px',
                background: 'rgba(247,166,79,0.1)', border: '1px solid rgba(247,166,79,0.3)',
                borderRadius: 'var(--radius-md)', padding: '12px 14px',
              }}>
                <AlertTriangle size={16} style={{ color: 'var(--warning)', flexShrink: 0, marginTop: '1px' }} />
                <span style={{ fontSize: '0.8rem', color: 'var(--warning)', lineHeight: 1.5 }}>
                  {captureStatus.warning}
                </span>
              </div>
            )}

            {/* Capture backend info */}
            <div style={{
              display: 'flex', alignItems: 'center', gap: '10px',
              background: 'var(--bg-surface)', borderRadius: 'var(--radius-sm)',
              padding: '8px 12px', fontSize: '0.78rem',
            }}>
              <Cpu size={13} style={{ color: 'var(--accent)' }} />
              <span style={{ color: 'var(--text-muted)' }}>Capture backend:</span>
              <span style={{ color: 'var(--accent)', fontWeight: 600 }}>{captureStatus.backend}</span>
              {captureStatus.renderSuspended && (
                <span className="badge badge-warning" style={{ fontSize: '0.7rem', marginLeft: 'auto' }}>⚠ Render Paused</span>
              )}
              {captureStatus.isStale && !captureStatus.renderSuspended && (
                <span className="badge badge-inactive" style={{ fontSize: '0.7rem', marginLeft: 'auto' }}>Last Frame</span>
              )}
            </div>

            {/* Pairing code */}
            <div className="card" style={{ textAlign: 'center', gap: '12px', display: 'flex', flexDirection: 'column' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px', justifyContent: 'center', color: 'var(--text-secondary)' }}>
                <Key size={14} />
                <span style={{ fontSize: '0.8rem' }}>Pairing Code</span>
                {codeTimer > 0 && <span style={{ fontSize: '0.72rem', color: 'var(--text-muted)' }}>expires in {codeTimer}s</span>}
              </div>
              <div className="pairing-code">{pairingCode ?? '------'}</div>
              <button className="btn btn-ghost btn-sm" onClick={generatePairingCode}>Generate new code</button>
            </div>

            {/* Connected clients */}
            <div>
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '10px' }}>
                <Users size={14} />
                <h3>Connected Devices ({connectedClients.length})</h3>
              </div>
              {connectedClients.length === 0 ? (
                <div className="empty-state" style={{ padding: '20px' }}>
                  <p>Waiting for clients… Share the code above</p>
                </div>
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
                  {connectedClients.map(client => (
                    <div key={client.client_id} className="device-card">
                      <div>
                        <div style={{ fontWeight: 500, fontSize: '0.875rem' }}>{client.display_name}</div>
                        <div style={{ fontSize: '0.75rem', color: 'var(--text-muted)' }}>
                          {client.addr} · {client.permissions.input_control ? 'Full Control' : 'View Only'}
                        </div>
                      </div>
                      <div style={{ display: 'flex', gap: '8px' }}>
                        <span className="badge badge-active">{client.stats.fps.toFixed(0)} fps</span>
                        <button className="btn btn-danger btn-sm" onClick={() => kickClient(client.client_id)}>Kick</button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <button id="btn-stop-share" className="btn btn-danger btn-lg btn-full" onClick={stopShare}>
              <Square size={16} /> Stop Sharing
            </button>
          </>
        )}
      </div>

      {/* Stats bar */}
      {isSharing && (
        <div className="stats-bar">
          {/* FPS — colour-coded */}
          <div className="stat-item">
            FPS
            <span
              className="stat-value"
              style={{
                color: stats.fps >= 55 ? 'var(--success)'
                     : stats.fps >= 30 ? 'var(--warning)'
                     : 'var(--danger)',
              }}
            >
              {stats.fps.toFixed(0)}
            </span>
          </div>

          <div className="stat-item">Enc <span className="stat-value">{stats.encode_ms ?? 0}ms</span></div>
          <div className="stat-item">Net <span className="stat-value">{stats.latency_ms}ms</span></div>
          <div className="stat-item">Bitrate <span className="stat-value">{(stats.bitrate_kbps / 1000).toFixed(1)} Mbps</span></div>
          <div className="stat-item">Clients <span className="stat-value">{stats.client_count}</span></div>

          {/* GPU / CPU badge */}
          <div
            className="stat-item"
            title={stats.gpu_path_active ? 'GPU zero-copy texture path is active' : 'CPU software encode path'}
            style={{ marginLeft: 'auto', cursor: 'default' }}
          >
            {stats.gpu_path_active ? (
              <span style={{
                display: 'flex', alignItems: 'center', gap: '4px',
                color: 'var(--success)', fontWeight: 600, fontSize: '0.72rem',
              }}>
                <Zap size={11} /> GPU
              </span>
            ) : (
              <span style={{
                display: 'flex', alignItems: 'center', gap: '4px',
                color: 'var(--text-muted)', fontWeight: 500, fontSize: '0.72rem',
              }}>
                <Cpu size={11} /> CPU
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  );
};
