import React, { useEffect, useState, useRef, useCallback } from 'react';
import { invoke, listen } from '../store/ipc';
import * as QRCode from 'qrcode';
import { 
  ArrowLeft, Play, Square, RefreshCw, Users, Key, AlertTriangle, 
  Cpu, Zap, Settings as SettingsIcon, Monitor, Activity, Terminal, ShieldAlert
} from 'lucide-react';
import type { Page } from '../App';
import { useSessionStore, type WindowInfo } from '../store/sessionStore';
import { useToastStore } from '../store/toastStore';
import { DebugOverlay } from '../components/DebugOverlay';

interface HostProps { onNavigate: (p: Page) => void; }

interface ParsedLog {
  timestamp?: string;
  level?: string;
  message?: string;
  target?: string;
  raw: string;
}

export const Host: React.FC<HostProps> = ({ onNavigate }) => {
  const {
    availableWindows, windowsLoading, fetchWindows,
    availableMonitors, monitorsLoading, fetchMonitors,
    isSharing, activeHwnd, activeTarget, startShare, stopShare,
    pairingCode, pairingExpiresIn, generatePairingCode,
    connectedClients, kickClient, fetchActiveClients,
    stats, updateStats,
    encoderInfo, setEncoderInfo,
    fetchActiveShare,
  } = useSessionStore();

  const { addToast } = useToastStore();

  const [tab, setTab] = useState<'share' | 'devices' | 'performance' | 'logs'>('share');
  const [selectedHwnd, setSelectedHwnd] = useState<number | null>(null);
  const [selectedMonitor, setSelectedMonitor] = useState<number | null>(null);
  const [selectedHwnds, setSelectedHwnds] = useState<number[]>([]);
  const [shareMode, setShareMode] = useState<'single' | 'display' | 'multi' | 'dual' | 'all_displays'>('single');
  const [codeTimer, setCodeTimer] = useState(0);
  const [windowSearch, setWindowSearch] = useState('');
  const [hostIps, setHostIps] = useState<string[]>([]);
  const qrCanvasRef = useRef<HTMLCanvasElement>(null);

  // Fetch host IPs on mount
  useEffect(() => {
    invoke<string[]>('get_host_ips')
      .then(ips => {
        setHostIps(ips.filter(ip => ip !== '127.0.0.1'));
      })
      .catch(console.error);
  }, []);

  // Render QR code to canvas
  useEffect(() => {
    if (qrCanvasRef.current && pairingCode && hostIps.length > 0) {
      const payload = JSON.stringify({
        ips: hostIps,
        port: 45101,
        code: pairingCode
      });
      QRCode.toCanvas(qrCanvasRef.current, payload, {
        width: 120,
        margin: 1,
        color: {
          dark: '#000000',
          light: '#FFFFFF'
        }
      }).catch(console.error);
    }
  }, [pairingCode, hostIps]);
  
  // Real-time logs state
  const [logs, setLogs] = useState<string[]>([]);
  const [logType, setLogType] = useState<string>('service');
  const [logSearch, setLogSearch] = useState('');
  
  // Stats history for chart
  const [statsHistory, setStatsHistory] = useState<{ fps: number; encode: number; latency: number }[]>([]);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const logsEndRef = useRef<HTMLDivElement>(null);

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

  // Auto-retry and periodic refresh of windows / monitors list
  useEffect(() => {
    let cancelled = false;
    let retries = 0;
    const maxRetries = 6;
    let intervalId: ReturnType<typeof setInterval> | null = null;

    const tryFetch = async () => {
      while (!cancelled && retries < maxRetries) {
        try {
          if (shareMode === 'display' || shareMode === 'all_displays') {
            await fetchMonitors();
            const state = useSessionStore.getState();
            if (state.availableMonitors.length > 0) {
              setLoadError(null);
              break;
            }
          } else {
            await fetchWindows();
            const state = useSessionStore.getState();
            if (state.availableWindows.length > 0) {
              setLoadError(null);
              break;
            }
          }
        } catch {
          // ignore
        }
        retries++;
        if (retries < maxRetries) {
          setLoadError(`Connecting to service… (attempt ${retries}/${maxRetries})`);
          await new Promise(r => setTimeout(r, 1500));
        }
      }
      if (!cancelled) {
        setLoadError(null);
        // Start periodic refresh when on the share tab and not sharing
        intervalId = setInterval(() => {
          const state = useSessionStore.getState();
          if (!state.isSharing && tab === 'share') {
            if (shareMode === 'display' || shareMode === 'all_displays') {
              fetchMonitors().catch(console.error);
            } else {
              fetchWindows().catch(console.error);
            }
          }
        }, 4000);
      }
    };

    tryFetch();

    return () => {
      cancelled = true;
      if (intervalId) clearInterval(intervalId);
    };
  }, [fetchWindows, fetchMonitors, tab, shareMode]);

  // Capture backend change listener
  useEffect(() => {
    const unlisten = listen<{ to: string; reason: string }>('capture_backend_switched', ev => {
      setCaptureStatus(s => ({ ...s, backend: ev.payload.to }));
      addToast('Backend Switched', `Capture engine changed to ${ev.payload.to}`, 'info');
    });
    return () => { unlisten.then(f => f()); };
  }, [addToast]);

  // Fetch active share status on mount/tab switch
  useEffect(() => {
    if (tab === 'share') {
      fetchActiveShare().catch(console.error);
    }
  }, [tab, fetchActiveShare]);

  // Auto-select primary monitor when displays list is populated
  useEffect(() => {
    if (shareMode === 'display' && availableMonitors.length > 0 && selectedMonitor === null) {
      const primary = availableMonitors.find(m => m.is_primary);
      if (primary) {
        setSelectedMonitor(primary.handle);
      } else {
        setSelectedMonitor(availableMonitors[0].handle);
      }
    }
  }, [shareMode, availableMonitors, selectedMonitor]);

  // NOTE: Stats, encoder_ready, pairing_code, client_connected/disconnected,
  // share_started/stopped are all handled globally by useGlobalIpcEvents() in App.tsx.
  // Host-specific events (capture_backend_switched) are handled above.

  // Expiry Timer countdown
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

  // Logs polling
  const fetchLogs = useCallback(async () => {
    try {
      const lines = await invoke<string[]>('read_recent_logs', { logType, limit: 120 });
      setLogs(lines);
    } catch (e) {
      console.error('Failed to read logs:', e);
    }
  }, [logType]);

  useEffect(() => {
    if (tab !== 'logs') return;
    fetchLogs();
    const iv = setInterval(fetchLogs, 1500);
    return () => clearInterval(iv);
  }, [fetchLogs, tab]);

  // Active Clients polling when on the devices tab
  useEffect(() => {
    if (tab !== 'devices') return;
    fetchActiveClients();
    const iv = setInterval(fetchActiveClients, 2000);
    return () => clearInterval(iv);
  }, [fetchActiveClients, tab]);

  // Scroll to bottom of logs console
  useEffect(() => {
    if (logsEndRef.current) {
      logsEndRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [logs]);

  // Performance stats accumulator
  useEffect(() => {
    if (isSharing) {
      setStatsHistory(h => {
        const next = [...h, { fps: stats.fps, encode: stats.encode_ms, latency: stats.latency_ms }];
        return next.length > 60 ? next.slice(-60) : next;
      });
    } else {
      setStatsHistory([]);
    }
  }, [stats.fps, stats.encode_ms, stats.latency_ms, isSharing]);

  // Performance chart renderer
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || statsHistory.length < 2) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);

    // Grid lines
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.05)';
    ctx.lineWidth = 1;
    for (let i = 1; i < 4; i++) {
      const y = (h / 4) * i;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
    }

    // Draw FPS (Green)
    ctx.beginPath();
    ctx.strokeStyle = '#10b981';
    ctx.lineWidth = 2;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.fps, 75) / 75) * (h - 20) - 10;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();

    // Draw Encoder Latency (Yellow)
    ctx.beginPath();
    ctx.strokeStyle = '#f59e0b';
    ctx.lineWidth = 1.5;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.encode, 20) / 20) * (h - 20) - 10;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();

    // Draw Network Latency (Blue)
    ctx.beginPath();
    ctx.strokeStyle = '#3b82f6';
    ctx.lineWidth = 1.5;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.latency, 40) / 40) * (h - 20) - 10;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
  }, [statsHistory]);

  const handleStartShare = async () => {
    let target: any = null;
    if (shareMode === 'single') {
      if (!selectedHwnd) return;
      const win = availableWindows.find(w => w.hwnd === selectedHwnd);
      if (win?.suspends_render_when_minimized) {
        setCaptureStatus(s => ({
          ...s,
          appKind: win.app_kind ?? 'Unknown',
          warning: `${win.process_name} may pause rendering when minimized. Beacon will serve the last frame until rendering resumes.`,
        }));
      }
      target = { kind: 'window', data: selectedHwnd };
    } else if (shareMode === 'display') {
      if (!selectedMonitor) return;
      target = { kind: 'display', data: selectedMonitor };
    } else if (shareMode === 'all_displays') {
      const handles = availableMonitors.map(m => m.handle);
      target = { kind: 'multi_display', data: handles };
    } else if (shareMode === 'multi') {
      if (selectedHwnds.length === 0) return;
      target = { kind: 'multi_window', data: selectedHwnds };
    } else if (shareMode === 'dual') {
      if (selectedHwnds.length !== 2) {
        addToast('Selection Error', 'Dual Window mode requires exactly two windows.', 'error');
        return;
      }
      target = { kind: 'dual_window', data: [selectedHwnds[0], selectedHwnds[1]] };
    }

    if (!target) return;
    await startShare(target);
    await generatePairingCode();
  };

  const parseLogLine = (line: string): ParsedLog => {
    try {
      const data = JSON.parse(line);
      const msg = data.fields?.message || data.message || line;
      return {
        timestamp: data.timestamp ? new Date(data.timestamp).toLocaleTimeString() : undefined,
        level: data.level,
        message: msg,
        target: data.target,
        raw: line,
      };
    } catch {
      return {
        raw: line,
        message: line,
      };
    }
  };

  const getWindowIcon = (win: WindowInfo) => {
    const name = win.process_name.toLowerCase();
    if (name.includes('chrome') || name.includes('edge') || name.includes('browser')) return '🌐';
    if (name.includes('code') || name.includes('studio') || name.includes('visual')) return '💻';
    if (name.includes('notepad') || name.includes('word') || name.includes('text')) return '📝';
    if (name.includes('explorer') || name.includes('files')) return '📁';
    if (name.includes('discord') || name.includes('slack') || name.includes('teams')) return '💬';
    if (name.includes('player') || name.includes('media') || name.includes('vlc')) return '🎬';
    return '🖥️';
  };

  const toggleHwnd = (hwnd: number) => {
    setSelectedHwnds(prev =>
      prev.includes(hwnd) ? prev.filter(h => h !== hwnd) : [...prev, hwnd]
    );
  };

  const toggleDualHwnd = (hwnd: number) => {
    setSelectedHwnds(prev => {
      if (prev.includes(hwnd)) {
        return prev.filter(h => h !== hwnd);
      }
      if (prev.length >= 2) {
        return [prev[1], hwnd];
      }
      return [...prev, hwnd];
    });
  };

  // Window list search query
  const filteredWindows = availableWindows.filter(win => 
    win.title.toLowerCase().includes(windowSearch.toLowerCase()) ||
    win.process_name.toLowerCase().includes(windowSearch.toLowerCase())
  );

  return (
    <div className="dashboard-container">
      {/* Sidebar Navigation */}
      <div className="sidebar">
        <div className="sidebar-header">
          <Monitor size={22} style={{ color: 'var(--accent)' }} />
          <h2>Beacon Host</h2>
        </div>

        <div className="sidebar-nav">
          <div className={`sidebar-item ${tab === 'share' ? 'active' : ''}`} onClick={() => setTab('share')}>
            <Play size={16} />
            <span>Share Screen</span>
          </div>
          
          <div className={`sidebar-item ${tab === 'devices' ? 'active' : ''}`} onClick={() => setTab('devices')}>
            <Users size={16} />
            <span style={{ display: 'flex', alignItems: 'center', gap: '6px', width: '100%' }}>
              Devices
              {connectedClients.length > 0 && (
                <span className="badge badge-active" style={{ marginLeft: 'auto', padding: '1px 6px', fontSize: '0.68rem' }}>
                  {connectedClients.length}
                </span>
              )}
            </span>
          </div>

          <div className={`sidebar-item ${tab === 'performance' ? 'active' : ''}`} onClick={() => setTab('performance')}>
            <Activity size={16} />
            <span>Performance</span>
          </div>

          <div className={`sidebar-item ${tab === 'logs' ? 'active' : ''}`} onClick={() => setTab('logs')}>
            <Terminal size={16} />
            <span>System Logs</span>
          </div>
        </div>

        <div className="sidebar-footer">
          <button className="btn btn-ghost btn-sm btn-full" onClick={() => onNavigate('settings')}>
            <SettingsIcon size={14} /> Settings
          </button>
          
          {import.meta.env.MODE !== 'host' && (
            <button className="btn btn-ghost btn-sm btn-full" onClick={() => onNavigate('home')}>
              <ArrowLeft size={14} /> Main Menu
            </button>
          )}
        </div>
      </div>

      {/* Main Dashboard Space */}
      <div className="main-content">
        <div className="page-header">
          <h2 style={{ textTransform: 'capitalize' }}>
            {tab === 'share' ? 'Capture Engine' : tab === 'devices' ? 'Connected Devices' : tab === 'performance' ? 'Performance Analytics' : 'System Logs'}
          </h2>
          {isSharing && (
            <div className="badge badge-active" style={{ marginLeft: 'auto' }}>
              <div className="pulse-dot" /> Broadcasting
            </div>
          )}
        </div>

        <div className="page-content">
          
          {/* TAB 1: Share Panel */}
          {tab === 'share' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '20px', height: '100%' }}>
              {!isSharing ? (
                <>
                  {/* Share Mode Segmented Picker */}
                  <div style={{
                    display: 'flex',
                    background: 'rgba(255, 255, 255, 0.03)',
                    border: '1px solid var(--border)',
                    borderRadius: 'var(--radius-md)',
                    padding: '4px',
                    gap: '4px',
                    marginBottom: '6px'
                  }}>
                    {[
                      { id: 'single', label: 'Single Window', icon: '🪟' },
                      { id: 'display', label: 'Entire Display', icon: '🖥️' },
                      { id: 'all_displays', label: 'All Displays', icon: '🖥️🖥️' },
                      { id: 'multi', label: 'Multi-Window', icon: '🥞' },
                      { id: 'dual', label: 'Dual Window', icon: '👥' },
                    ].map(mode => (
                      <button
                        key={mode.id}
                        className={`btn btn-sm ${shareMode === mode.id ? 'btn-primary' : 'btn-ghost'}`}
                        style={{
                          flex: 1,
                          border: 'none',
                          boxShadow: shareMode === mode.id ? 'var(--shadow-sm)' : 'none',
                          padding: '8px 12px',
                        }}
                        onClick={() => {
                          setShareMode(mode.id as any);
                          setSelectedHwnd(null);
                          setSelectedHwnds([]);
                          setSelectedMonitor(null);
                        }}
                      >
                        <span style={{ marginRight: '6px' }}>{mode.icon}</span>
                        {mode.label}
                      </button>
                    ))}
                  </div>

                  {shareMode !== 'display' && shareMode !== 'all_displays' ? (
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '14px' }}>
                      <div style={{ flex: 1, display: 'flex', alignItems: 'center', gap: '12px' }}>
                        <input
                          type="text"
                          placeholder="Search open windows..."
                          value={windowSearch}
                          onChange={e => setWindowSearch(e.target.value)}
                          style={{ maxWidth: '360px' }}
                        />
                        {shareMode === 'dual' && (
                          <span className={`badge ${selectedHwnds.length === 2 ? 'badge-active' : 'badge-warning'}`} style={{ padding: '6px 12px' }}>
                            Selected: {selectedHwnds.length}/2
                          </span>
                        )}
                        {shareMode === 'multi' && (
                          <span className={`badge ${selectedHwnds.length > 0 ? 'badge-active' : 'badge-inactive'}`} style={{ padding: '6px 12px' }}>
                            Selected: {selectedHwnds.length}
                          </span>
                        )}
                      </div>
                      <button className="btn btn-ghost btn-sm" onClick={fetchWindows} disabled={windowsLoading}>
                        <RefreshCw size={14} className={windowsLoading ? 'spinner' : ''} />
                        Refresh List
                      </button>
                    </div>
                  ) : (
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '14px' }}>
                      <div style={{ flex: 1 }}>
                        <p style={{ fontSize: '0.85rem', color: 'var(--text-secondary)' }}>
                          Select an entire display monitor to broadcast. Supports multi-monitor setups.
                        </p>
                      </div>
                      <button className="btn btn-ghost btn-sm" onClick={fetchMonitors} disabled={monitorsLoading}>
                        <RefreshCw size={14} className={monitorsLoading ? 'spinner' : ''} />
                        Refresh Monitors
                      </button>
                    </div>
                  )}

                  {/* Mode UI Renders */}
                  {shareMode === 'display' || shareMode === 'all_displays' ? (
                    monitorsLoading ? (
                      <div className="empty-state" style={{ flex: 1 }}>
                        <div className="spinner" />
                        <p>{loadError || 'Scanning connected monitor devices...'}</p>
                      </div>
                    ) : availableMonitors.length === 0 ? (
                      <div className="empty-state" style={{ flex: 1 }}>
                        <span className="empty-state-icon">🖥️</span>
                        <p>No active display monitors discovered</p>
                        <button className="btn btn-ghost btn-sm" onClick={fetchMonitors} style={{ marginTop: '8px' }}>
                          <RefreshCw size={12} /> Retry Scan
                        </button>
                      </div>
                    ) : (
                      <div style={{ display: 'flex', flexDirection: 'column', gap: '12px', flex: 1 }}>
                        {shareMode === 'all_displays' && (
                          <div style={{
                            padding: '12px',
                            background: 'rgba(255, 255, 255, 0.02)',
                            border: '1px solid var(--border)',
                            borderRadius: 'var(--radius-md)',
                            fontSize: '0.85rem',
                            color: 'var(--text-secondary)'
                          }}>
                            All active display monitors will be streamed simultaneously in a responsive grid.
                          </div>
                        )}
                        <div className="window-grid" style={{ overflowY: 'auto', flex: 1, maxHeight: 'calc(100vh - 320px)' }}>
                          {availableMonitors.map(mon => (
                            <div
                              key={mon.handle}
                              id={`monitor-card-${mon.handle}`}
                              className={`window-card ${shareMode === 'all_displays' ? 'active' : selectedMonitor === mon.handle ? 'selected' : ''}`}
                              onClick={() => {
                                if (shareMode !== 'all_displays') {
                                  setSelectedMonitor(mon.handle);
                                }
                              }}
                              style={{
                                cursor: shareMode === 'all_displays' ? 'default' : 'pointer',
                                opacity: shareMode === 'all_displays' ? 0.9 : 1
                              }}
                            >
                              <div className="window-thumb">🖥️</div>
                              <div className="window-info">
                                <span className="window-title" style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                                  Display {mon.index}
                                  {mon.is_primary && (
                                    <span className="badge badge-active" style={{ fontSize: '0.62rem', padding: '1px 5px', textTransform: 'uppercase' }}>
                                      Primary
                                    </span>
                                  )}
                                </span>
                                <span className="window-process">{mon.name}</span>
                                <span className="window-process" style={{ fontWeight: 600, color: 'var(--text-primary)', marginTop: '4px' }}>
                                  {mon.width}×{mon.height} @ {mon.refresh_rate}Hz
                                </span>
                              </div>
                            </div>
                          ))}
                        </div>
                      </div>
                    )
                  ) : (
                    windowsLoading ? (
                      <div className="empty-state" style={{ flex: 1 }}>
                        <div className="spinner" />
                        <p>{loadError || 'Scanning local Windows handles...'}</p>
                      </div>
                    ) : filteredWindows.length === 0 ? (
                      <div className="empty-state" style={{ flex: 1 }}>
                        <span className="empty-state-icon">🪟</span>
                        <p>{windowSearch ? 'No windows match your query' : 'No capture-eligible windows found'}</p>
                        <button className="btn btn-ghost btn-sm" onClick={fetchWindows} style={{ marginTop: '8px' }}>
                          <RefreshCw size={12} /> Retry Scan
                        </button>
                      </div>
                    ) : (
                      <div className="window-grid" style={{ overflowY: 'auto', flex: 1, maxHeight: 'calc(100vh - 280px)' }}>
                        {filteredWindows.map(win => {
                          const isChecked = shareMode === 'single'
                            ? selectedHwnd === win.hwnd
                            : selectedHwnds.includes(win.hwnd);

                          return (
                            <div
                              key={win.hwnd}
                              id={`window-card-${win.hwnd}`}
                              className={`window-card ${isChecked ? 'selected' : ''}`}
                              onClick={() => {
                                if (shareMode === 'single') {
                                  setSelectedHwnd(win.hwnd);
                                } else if (shareMode === 'dual') {
                                  toggleDualHwnd(win.hwnd);
                                } else {
                                  toggleHwnd(win.hwnd);
                                }
                              }}
                            >
                              <div style={{ position: 'relative' }}>
                                <div className="window-thumb">{getWindowIcon(win)}</div>
                                {isChecked && (
                                  <div style={{
                                    position: 'absolute', top: '6px', right: '6px',
                                    background: 'var(--success)', color: '#fff',
                                    borderRadius: '50%', width: '20px', height: '20px',
                                    display: 'flex', alignItems: 'center', justifyContent: 'center',
                                    fontSize: '0.75rem', fontWeight: 'bold',
                                    boxShadow: 'var(--shadow-sm)',
                                    border: '1px solid rgba(255,255,255,0.2)'
                                  }}>✓</div>
                                )}
                              </div>
                              <div className="window-info">
                                <span className="window-title" title={win.title}>{win.title}</span>
                                <span className="window-process">{win.process_name} · {win.width}×{win.height}</span>
                                <div style={{ display: 'flex', gap: '4px', flexWrap: 'wrap', marginTop: '4px' }}>
                                  {win.is_minimized && <span className="badge badge-warning" style={{ fontSize: '0.65rem', padding: '1px 6px' }}>Minimized</span>}
                                  <span className="badge badge-inactive" style={{ fontSize: '0.65rem', padding: '1px 6px' }}>{win.app_kind}</span>
                                  {win.suspends_render_when_minimized && (
                                    <span title="This app stops rendering when minimized"
                                      style={{ fontSize: '0.7rem', color: 'var(--warning)', display: 'flex', alignItems: 'center', gap: '2px' }}>
                                      ⚠ Suspends
                                    </span>
                                  )}
                                </div>
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    )
                  )}

                  <button
                    id="btn-start-share"
                    className="btn btn-success btn-lg btn-full"
                    disabled={
                      (shareMode === 'single' && !selectedHwnd) ||
                      (shareMode === 'display' && !selectedMonitor) ||
                      (shareMode === 'multi' && selectedHwnds.length === 0) ||
                      (shareMode === 'dual' && selectedHwnds.length !== 2) ||
                      (shareMode === 'all_displays' && availableMonitors.length === 0) ||
                      windowsLoading || monitorsLoading
                    }
                    onClick={handleStartShare}
                    style={{ marginTop: 'auto' }}
                  >
                    <Play size={16} />
                    {shareMode === 'single' && 'Broadcast Selected Window'}
                    {shareMode === 'display' && 'Broadcast Entire Display'}
                    {shareMode === 'all_displays' && 'Broadcast All Displays'}
                    {shareMode === 'multi' && `Broadcast Selected Windows (${selectedHwnds.length})`}
                    {shareMode === 'dual' && `Broadcast Dual Windows (${selectedHwnds.length}/2)`}
                  </button>
                </>
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: '20px' }}>
                  {/* Backend Warning */}
                  {captureStatus.warning && (
                    <div className="glass-panel" style={{
                      display: 'flex', alignItems: 'flex-start', gap: '12px',
                      background: 'rgba(245,158,11,0.06)', border: '1px solid rgba(245,158,11,0.25)',
                      borderRadius: 'var(--radius-md)', padding: '14px 16px',
                    }}>
                      <AlertTriangle size={18} style={{ color: 'var(--warning)', flexShrink: 0 }} />
                      <span style={{ fontSize: '0.820rem', color: 'var(--warning)', lineHeight: 1.5 }}>
                        {captureStatus.warning}
                      </span>
                    </div>
                  )}

                  {/* active panel details */}
                  <div className="grid-2" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '20px' }}>
                    <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '14px', alignItems: 'center', justifyContent: 'center' }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--text-secondary)' }}>
                        <Key size={16} />
                        <span style={{ fontSize: '0.875rem' }}>Scan QR or Enter Code</span>
                        {codeTimer > 0 && <span style={{ fontSize: '0.72rem', color: 'var(--text-muted)' }}>({codeTimer}s)</span>}
                      </div>
                      <div style={{ display: 'flex', alignItems: 'center', gap: '20px', justifyContent: 'center', width: '100%' }}>
                        <div className="pairing-code" style={{ minWidth: '120px', textAlign: 'center' }}>{pairingCode ?? '------'}</div>
                        {pairingCode && (
                          <div style={{ background: '#fff', padding: '6px', borderRadius: '8px', display: 'flex', alignItems: 'center', justifyContent: 'center', boxShadow: '0 4px 6px -1px rgba(0,0,0,0.1)' }}>
                            <canvas ref={qrCanvasRef} style={{ width: '100px', height: '100px', display: 'block' }} />
                          </div>
                        )}
                      </div>
                      <button className="btn btn-ghost btn-sm" onClick={generatePairingCode}>Cycle Pairing Code</button>
                    </div>

                    <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                      <h3 style={{ borderBottom: '1px solid var(--border)', paddingBottom: '6px' }}>Streaming Session</h3>
                      
                      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.85rem' }}>
                        <span style={{ color: 'var(--text-secondary)' }}>Sharing Mode:</span>
                        <span style={{ color: 'var(--accent)', fontWeight: 600 }}>
                          {activeTarget?.kind === 'window' ? 'Single Window' :
                           activeTarget?.kind === 'display' ? 'Entire Display' :
                           activeTarget?.kind === 'multi_window' ? 'Multi-Window' :
                           activeTarget?.kind === 'dual_window' ? 'Dual Window' :
                           activeHwnd ? 'Single Window' : 'Unknown'}
                        </span>
                      </div>

                      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.85rem' }}>
                        <span style={{ color: 'var(--text-secondary)' }}>Target Info:</span>
                        <span style={{ fontFamily: 'monospace', textOverflow: 'ellipsis', overflow: 'hidden', whiteSpace: 'nowrap', maxWidth: '180px' }}>
                          {activeTarget?.kind === 'window' ? `HWND ${activeTarget.data}` :
                           activeTarget?.kind === 'display' ? `Display Handle ${activeTarget.data}` :
                           activeTarget?.kind === 'multi_window' ? `${activeTarget.data.length} Windows` :
                           activeTarget?.kind === 'dual_window' ? '2 Windows' :
                           activeHwnd ? `HWND ${activeHwnd}` : 'Unknown'}
                        </span>
                      </div>

                      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.85rem' }}>
                        <span style={{ color: 'var(--text-secondary)' }}>Capture Engine:</span>
                        <span style={{ color: 'var(--accent)', fontWeight: 600 }}>{captureStatus.backend}</span>
                      </div>

                      {encoderInfo && (
                        <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.85rem' }}>
                          <span style={{ color: 'var(--text-secondary)' }}>GPU Encoder:</span>
                          <span style={{ color: encoderInfo.hw_accelerated ? 'var(--success)' : 'var(--text-secondary)' }}>
                            {encoderInfo.encoder_name} ({encoderInfo.hw_accelerated ? 'HW Accelerated' : 'Software'})
                          </span>
                        </div>
                      )}

                      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.85rem' }}>
                        <span style={{ color: 'var(--text-secondary)' }}>Zero-Copy Mode:</span>
                        <span style={{ color: stats.gpu_path_active ? 'var(--success)' : 'var(--warning)' }}>
                          {stats.gpu_path_active ? 'Enabled' : 'CPU Fallback'}
                        </span>
                      </div>
                    </div>
                  </div>

                  <button id="btn-stop-share" className="btn btn-danger btn-lg btn-full" onClick={stopShare}>
                    <Square size={16} /> Terminate Broadcast Stream
                  </button>
                </div>
              )}
            </div>
          )}

          {/* TAB 2: Devices */}
          {tab === 'devices' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '14px' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                <Users size={16} />
                <h3>Active Client Viewers ({connectedClients.length})</h3>
              </div>
              
              {connectedClients.length === 0 ? (
                <div className="empty-state" style={{ padding: '36px' }}>
                  <p>No connections established. Share the 6-digit pairing code on page tab 1 to allow client joining.</p>
                </div>
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
                  {connectedClients.map(client => (
                    <div key={client.client_id} className="device-card">
                      <div>
                        <div style={{ fontWeight: 600, fontSize: '0.9rem', color: '#fff' }}>{client.display_name}</div>
                        <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
                          IP: {client.addr} · Input Permission: {client.permissions.input_control ? 'Allow Control' : 'Read Only'}
                        </div>
                      </div>
                      <div style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
                        <span className="badge badge-active">{client.stats.fps.toFixed(0)} FPS</span>
                        <button className="btn btn-danger btn-sm" onClick={() => kickClient(client.client_id)}>Kick Connection</button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* TAB 3: Performance */}
          {tab === 'performance' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '20px' }}>
              {isSharing ? (
                <>
                  {/* Dashboard stats cards */}
                  <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: '14px' }}>
                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Stream Frame Rate</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: 'var(--success)', marginTop: '6px' }}>
                        {stats.fps.toFixed(0)} <span style={{ fontSize: '0.9rem' }}>FPS</span>
                      </div>
                    </div>
                    
                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Encode Delay</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: stats.encode_ms > 15 ? 'var(--warning)' : 'var(--text-primary)', marginTop: '6px' }}>
                        {stats.encode_ms} <span style={{ fontSize: '0.9rem' }}>ms</span>
                      </div>
                    </div>

                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Network RTT</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: stats.latency_ms > 50 ? 'var(--danger)' : 'var(--accent)', marginTop: '6px' }}>
                        {stats.latency_ms} <span style={{ fontSize: '0.9rem' }}>ms</span>
                      </div>
                    </div>

                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Bitrate Transfer</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: 'var(--accent-purple)', marginTop: '6px' }}>
                        {(stats.bitrate_kbps / 1000).toFixed(1)} <span style={{ fontSize: '0.9rem' }}>Mbps</span>
                      </div>
                    </div>
                  </div>

                  {/* Sparkline canvas graph */}
                  <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                      <h3>Real-Time Engine Graphs</h3>
                      <div style={{ display: 'flex', gap: '14px', fontSize: '0.75rem' }}>
                        <span style={{ color: '#10b981', display: 'flex', alignItems: 'center', gap: '4px' }}>■ FPS</span>
                        <span style={{ color: '#f59e0b', display: 'flex', alignItems: 'center', gap: '4px' }}>■ Encode</span>
                        <span style={{ color: '#3b82f6', display: 'flex', alignItems: 'center', gap: '4px' }}>■ Network</span>
                      </div>
                    </div>
                    <canvas ref={canvasRef} width={600} height={180} style={{ width: '100%', height: '180px', display: 'block', background: 'rgba(0,0,0,0.15)', borderRadius: 'var(--radius-md)' }} />
                  </div>

                  {/* Zero-Copy info card */}
                  <div className="card" style={{ display: 'flex', alignItems: 'center', gap: '14px', border: `1px solid ${stats.gpu_path_active ? '#10b98144' : '#f59e0b44'}` }}>
                    {stats.gpu_path_active ? (
                      <Zap size={24} style={{ color: 'var(--success)' }} />
                    ) : (
                      <Cpu size={24} style={{ color: 'var(--warning)' }} />
                    )}
                    <div>
                      <h4 style={{ fontWeight: 600 }}>Zero-Copy GPU Path</h4>
                      <p style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
                        {stats.gpu_path_active 
                          ? 'Active. Display frames transit directly from your GPU memory into the encoder without CPU readback copies, preserving system performance.'
                          : 'Inactive. Falling back to CPU texture rendering buffer. Ensure graphics drivers are updated to optimize LAN encoding.'
                        }
                      </p>
                    </div>
                  </div>
                </>
              ) : (
                <div className="empty-state">
                  <Activity size={32} style={{ opacity: 0.3 }} />
                  <p>Performance analytics will plot once a window share stream begins broadcasting.</p>
                </div>
              )}
            </div>
          )}

          {/* TAB 4: Logs */}
          {tab === 'logs' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '14px', height: '100%', minHeight: 'calc(100vh - 260px)' }}>
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '12px' }}>
                <div style={{ display: 'flex', gap: '8px' }}>
                  {['service', 'capture', 'network', 'metrics'].map(type => (
                    <button
                      key={type}
                      className={`btn btn-sm ${logType === type ? 'btn-primary' : 'btn-ghost'}`}
                      onClick={() => setLogType(type)}
                      style={{ textTransform: 'capitalize' }}
                    >
                      {type} Logs
                    </button>
                  ))}
                </div>
                <input
                  type="text"
                  placeholder="Filter logs by keyword..."
                  value={logSearch}
                  onChange={e => setLogSearch(e.target.value)}
                  style={{ maxWidth: '240px' }}
                />
              </div>

              {/* Console log box */}
              <div className="logs-console" style={{ flex: 1, maxHeight: 'calc(100vh - 330px)' }}>
                {logs.length === 0 ? (
                  <div style={{ color: 'var(--text-muted)', textAlign: 'center', padding: '20px' }}>Loading service logs...</div>
                ) : (
                  logs
                    .map(parseLogLine)
                    .filter(log => log.message?.toLowerCase().includes(logSearch.toLowerCase()) || (log.target?.toLowerCase() || '').includes(logSearch.toLowerCase()))
                    .map((log, index) => (
                      <div key={index} className={`log-entry ${(log.level || '').toLowerCase()}`}>
                        {log.timestamp && <span className="log-time">[{log.timestamp}]</span>}
                        {log.level && <span className="log-level" style={{ fontWeight: 600 }}>{log.level}</span>}
                        {log.target && <span className="log-target" style={{ color: 'var(--accent-purple)' }}>{log.target}:</span>}
                        <span className="log-text">{log.message}</span>
                      </div>
                    ))
                )}
                <div ref={logsEndRef} />
              </div>
            </div>
          )}

        </div>
      </div>

      {/* Debug Overlay listener trigger */}
      {isSharing && (
        <DebugOverlay 
          backend={captureStatus.backend} 
          sessionId={useSessionStore.getState().stats.gpu_path_active ? "GPU-Direct" : "CPU-Render"} 
        />
      )}
    </div>
  );
};
