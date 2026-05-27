import React, { useEffect, useRef, useState, useCallback } from 'react';
import { ArrowLeft, RefreshCw, Wifi, WifiOff, Maximize2, Minimize2, Activity, Settings as SettingsIcon, Play, Terminal, Users } from 'lucide-react';
import type { Page } from '../App';
import { useSessionStore, type DiscoveredHost } from '../store/sessionStore';
import { useToastStore } from '../store/toastStore';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface ClientProps { onNavigate: (p: Page) => void; }

interface DecoderStats {
  fps: number;
  decodeMs: number;
  frames: number;
  keyframes: number;
}

interface ParsedLog {
  timestamp?: string;
  level?: string;
  message?: string;
  target?: string;
  raw: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// WebCodecs decoder hook
// ─────────────────────────────────────────────────────────────────────────────

function useWebCodecsDecoder(canvasRef: React.RefObject<HTMLCanvasElement>) {
  const decoderRef = useRef<VideoDecoder | null>(null);
  const frameCountRef = useRef(0);
  const lastFpsCheck = useRef(Date.now());
  const [stats, setStats] = useState<DecoderStats>({ fps: 0, decodeMs: 0, frames: 0, keyframes: 0 });
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);

  const initDecoder = useCallback((width: number, height: number) => {
    if (decoderRef.current && decoderRef.current.state !== 'closed') {
      decoderRef.current.close();
    }

    const canvas = canvasRef.current;
    if (!canvas) return;
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d')!;

    const decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
        frameCountRef.current++;

        const now = Date.now();
        if (now - lastFpsCheck.current >= 1000) {
          const elapsed = (now - lastFpsCheck.current) / 1000;
          setStats(prev => ({
            ...prev,
            fps: Math.round(frameCountRef.current / elapsed),
            frames: prev.frames + frameCountRef.current,
          }));
          frameCountRef.current = 0;
          lastFpsCheck.current = now;
        }
      },
      error: (e: DOMException) => {
        console.error('[WebCodecs] Decoder error:', e);
        setError(`Decoder error: ${e.message}`);
      },
    });

    decoder.configure({
      codec: 'avc1.42C033',           // Constrained Baseline H.264, Level 5.1
      codedWidth: width,
      codedHeight: height,
      optimizeForLatency: true,        // Critical for streaming
      hardwareAcceleration: 'prefer-hardware',
    });

    decoderRef.current = decoder;
    setReady(true);
    setError(null);
  }, [canvasRef]);

  const feedChunk = useCallback((
    data: string,
    timestampUs: number,
    isKeyframe: boolean,
    width: number,
    height: number,
  ) => {
    if (!decoderRef.current || decoderRef.current.state === 'closed') {
      initDecoder(width, height);
    }
    const decoder = decoderRef.current!;
    if (decoder.state !== 'configured') return;

    const raw = atob(data);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);

    const chunk = new EncodedVideoChunk({
      type: isKeyframe ? 'key' : 'delta',
      timestamp: timestampUs,
      data: bytes,
    });

    try {
      const startDecode = performance.now();
      decoder.decode(chunk);
      const endDecode = performance.now();
      
      if (isKeyframe) {
        setStats(prev => ({ 
          ...prev, 
          keyframes: prev.keyframes + 1,
          decodeMs: Math.round(endDecode - startDecode)
        }));
      }
    } catch (e) {
      console.warn('[WebCodecs] decode() failed:', e);
    }
  }, [initDecoder]);

  const closeDecoder = useCallback(() => {
    if (decoderRef.current && decoderRef.current.state !== 'closed') {
      decoderRef.current.close();
      decoderRef.current = null;
    }
    setReady(false);
  }, []);

  return { feedChunk, closeDecoder, initDecoder, stats, error, ready };
}

// ─────────────────────────────────────────────────────────────────────────────
// Main Component
// ─────────────────────────────────────────────────────────────────────────────

export const Client: React.FC<ClientProps> = ({ onNavigate }) => {
  const {
    discoveredHosts, hostsLoading, discoverHosts,
    connectedHost, connectToHost, disconnectFromHost,
    stats: sessionStats,
  } = useSessionStore();

  const { addToast } = useToastStore();

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const { feedChunk, closeDecoder, stats: decodeStats, error: decodeError } =
    useWebCodecsDecoder(canvasRef as React.RefObject<HTMLCanvasElement>);

  const [tab, setTab] = useState<'join' | 'stream' | 'performance' | 'logs'>('join');
  const [pairingInput, setPairingInput] = useState('');
  const [connectingHost, setConnectingHost] = useState<DiscoveredHost | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [manualIp, setManualIp] = useState('');
  const [streamConnected, setStreamConnected] = useState(false);

  // Real-time client logs state
  const [logs, setLogs] = useState<string[]>([]);
  const [logType, setLogType] = useState<string>('service');
  const [logSearch, setLogSearch] = useState('');
  const logsEndRef = useRef<HTMLDivElement>(null);

  // Performance history accumulator
  const [statsHistory, setStatsHistory] = useState<{ fps: number; decode: number; latency: number }[]>([]);
  const chartCanvasRef = useRef<HTMLCanvasElement>(null);

  // Auto-discover hosts periodic scan
  useEffect(() => {
    let cancelled = false;
    let intervalId: ReturnType<typeof setInterval> | null = null;

    const runDiscovery = async () => {
      if (cancelled) return;
      try {
        await discoverHosts();
      } catch {
        // service not ready
      }
    };

    const initialScan = async () => {
      for (let i = 0; i < 6 && !cancelled; i++) {
        await runDiscovery();
        const { discoveredHosts: hosts } = useSessionStore.getState();
        if (hosts.length > 0) break;
        await new Promise(r => setTimeout(r, 2000));
      }
      if (!cancelled) {
        intervalId = setInterval(runDiscovery, 8000);
      }
    };

    initialScan();
    return () => {
      cancelled = true;
      if (intervalId) clearInterval(intervalId);
    };
  }, [discoverHosts]);

  // Listen for IPC push events from service
  useEffect(() => {
    const unlisten = listen<string>('service-event', (event) => {
      try {
        const ev = typeof event.payload === 'string'
          ? JSON.parse(event.payload)
          : event.payload;
        
        switch (ev.event) {
          case 'video_chunk':
            feedChunk(ev.data, ev.timestamp_us, ev.is_keyframe, ev.width, ev.height);
            break;
          case 'stream_connected':
            setStreamConnected(true);
            setTab('stream');
            setError(null);
            addToast('Stream Connected', 'Established direct connection with remote host.', 'success');
            break;
          case 'stream_disconnected':
            setStreamConnected(false);
            setTab('join');
            closeDecoder();
            addToast('Stream Disconnected', 'Connection with host terminated.', 'info');
            break;
          case 'error':
            setError(ev.message);
            addToast('Connection Error', ev.message, 'error');
            break;
        }
      } catch (e) {
        console.error('[IPC] Event parse error:', e);
      }
    });
    return () => { unlisten.then(f => f()); };
  }, [feedChunk, closeDecoder, addToast]);

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
    fetchLogs();
    const iv = setInterval(fetchLogs, 1500);
    return () => clearInterval(iv);
  }, [fetchLogs]);

  // Scroll logs to bottom
  useEffect(() => {
    if (logsEndRef.current) {
      logsEndRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [logs]);

  // Accumulate performance history
  useEffect(() => {
    if (streamConnected) {
      setStatsHistory(h => {
        const next = [...h, { fps: decodeStats.fps, decode: decodeStats.decodeMs, latency: sessionStats.latency_ms }];
        return next.length > 60 ? next.slice(-60) : next;
      });
    } else {
      setStatsHistory([]);
    }
  }, [decodeStats.fps, decodeStats.decodeMs, sessionStats.latency_ms, streamConnected]);

  // Render performance graph
  useEffect(() => {
    const canvas = chartCanvasRef.current;
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

    // Draw Decode FPS (Green)
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

    // Draw Decode Delay (Purple)
    ctx.beginPath();
    ctx.strokeStyle = '#818cf8';
    ctx.lineWidth = 1.5;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.decode, 20) / 20) * (h - 20) - 10;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();

    // Draw RTT Network Latency (Blue)
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

  // Input Forwarding event handlers
  const handleMouseMove = (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (!streamConnected) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    invoke('send_input', {
      event: {
        kind: 'mouse_move',
        x,
        y,
        viewport_w: Math.round(rect.width),
        viewport_h: Math.round(rect.height)
      }
    }).catch(console.error);
  };

  const handleMouseButton = (e: React.MouseEvent<HTMLCanvasElement>, pressed: boolean) => {
    if (!streamConnected) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;

    let btn = 0;
    if (e.button === 0) btn = 0;
    else if (e.button === 2) btn = 1;
    else if (e.button === 1) btn = 2;
    else return;

    invoke('send_input', {
      event: {
        kind: 'mouse_button',
        button: btn,
        pressed,
        x,
        y,
        viewport_w: Math.round(rect.width),
        viewport_h: Math.round(rect.height)
      }
    }).catch(console.error);
  };

  const handleWheel = (e: React.WheelEvent<HTMLCanvasElement>) => {
    if (!streamConnected) return;
    const deltaY = -Math.sign(e.deltaY);
    invoke('send_input', {
      event: {
        kind: 'mouse_scroll',
        delta_x: 0.0,
        delta_y: deltaY
      }
    }).catch(console.error);
  };

  // Keyboard Event Hooks
  useEffect(() => {
    if (!streamConnected) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      const keysToPrevent = ['Backspace', 'Tab', 'Space', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
      if (keysToPrevent.includes(e.code) || e.key === ' ') {
        e.preventDefault();
      }

      invoke('send_input', {
        event: {
          kind: 'key_press',
          vk_code: e.keyCode,
          scan_code: 0,
          pressed: true
        }
      }).catch(console.error);
    };

    const handleKeyUp = (e: KeyboardEvent) => {
      const keysToPrevent = ['Backspace', 'Tab', 'Space', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
      if (keysToPrevent.includes(e.code) || e.key === ' ') {
        e.preventDefault();
      }

      invoke('send_input', {
        event: {
          kind: 'key_press',
          vk_code: e.keyCode,
          scan_code: 0,
          pressed: false
        }
      }).catch(console.error);
    };

    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('keyup', handleKeyUp);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('keyup', handleKeyUp);
    };
  }, [streamConnected]);

  // Fullscreen toggle
  const toggleFullscreen = () => {
    const el = canvasRef.current?.parentElement;
    if (!el) return;
    if (!document.fullscreenElement) {
      el.requestFullscreen().then(() => setFullscreen(true));
    } else {
      document.exitFullscreen().then(() => setFullscreen(false));
    }
  };

  // Connection trigger
  const handleConnect = async (host: DiscoveredHost, skipPairingCheck = false) => {
    if (!skipPairingCheck && pairingInput && pairingInput.length !== 6) {
      setError('Pairing code must be exactly 6 digits.');
      return;
    }
    setError(null);
    setConnectingHost(host);

    try {
      await connectToHost(host, pairingInput);
      setStreamConnected(true);
      setTab('stream');
      setError(null);
      addToast('Stream Connected', 'Established direct connection with remote host.', 'success');
    } catch {
      setError('Connection refused. Please confirm code expiration.');
      addToast('Connection Failed', 'Unable to reach the host session.', 'error');
    }
    setConnectingHost(null);
  };

  const handleDisconnect = async () => {
    closeDecoder();
    setStreamConnected(false);
    disconnectFromHost();
    setTab('join');
  };

  const handleManualConnect = () => {
    if (!manualIp) return;
    const [ip, portStr] = manualIp.split(':');
    const port = parseInt(portStr || '45101');
    handleConnect({ name: ip, address: ip, port }, true);
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

  return (
    <div className="dashboard-container">
      {/* Sidebar Navigation */}
      <div className="sidebar">
        <div className="sidebar-header">
          <Wifi size={22} style={{ color: 'var(--accent-purple)' }} />
          <h2>Pulse Client</h2>
        </div>

        <div className="sidebar-nav">
          <div className={`sidebar-item ${tab === 'join' ? 'active' : ''}`} onClick={() => setTab('join')}>
            <Wifi size={16} />
            <span>Connect Hosts</span>
          </div>
          
          <div 
            className={`sidebar-item ${tab === 'stream' ? 'active' : ''} ${!streamConnected ? 'disabled' : ''}`}
            onClick={() => streamConnected && setTab('stream')}
            style={{ opacity: streamConnected ? 1 : 0.45, cursor: streamConnected ? 'pointer' : 'not-allowed' }}
          >
            <Play size={16} />
            <span>Active Stream</span>
          </div>

          <div 
            className={`sidebar-item ${tab === 'performance' ? 'active' : ''} ${!streamConnected ? 'disabled' : ''}`}
            onClick={() => streamConnected && setTab('performance')}
            style={{ opacity: streamConnected ? 1 : 0.45, cursor: streamConnected ? 'pointer' : 'not-allowed' }}
          >
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
          
          {import.meta.env.MODE !== 'player' && (
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
            {tab === 'join' ? 'Connect to Stream' : tab === 'stream' ? 'Video Stream HUD' : tab === 'performance' ? 'Decoding Latency Graph' : 'System Logs'}
          </h2>
          {streamConnected && (
            <div className="badge badge-active" style={{ marginLeft: 'auto' }}>
              <div className="pulse-dot" /> Live Receiver
            </div>
          )}
        </div>

        <div className="page-content" style={{ padding: tab === 'stream' ? '0' : '24px' }}>
          
          {/* TAB 1: Join / Discovery */}
          {tab === 'join' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '20px' }}>
              {/* Host List */}
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
                <h3>Local Network Discovery</h3>
                <button className="btn btn-ghost btn-sm" onClick={discoverHosts} disabled={hostsLoading}>
                  <RefreshCw size={14} className={hostsLoading ? 'spinner' : ''} />
                  Scan Network
                </button>
              </div>

              {discoveredHosts.length === 0 && hostsLoading ? (
                <div className="empty-state"><div className="spinner" /><p>Scanning subnet via mDNS & Broadcast...</p></div>
              ) : discoveredHosts.length === 0 ? (
                <div className="empty-state">
                  <WifiOff size={32} style={{ opacity: 0.3 }} />
                  <p>No active hosts discovered on local subnet</p>
                  <span style={{ fontSize: '0.78rem', color: 'var(--text-muted)' }}>
                    Confirm the Host has Beacon running in sharing mode.
                  </span>
                </div>
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
                  {discoveredHosts.map(host => (
                    <div key={host.address} className="device-card">
                      <div style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
                        <Wifi size={18} style={{ color: 'var(--accent-purple)' }} />
                        <div>
                          <div style={{ fontWeight: 600, fontSize: '0.9rem', color: '#fff' }}>{host.name}</div>
                          <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
                            IP Target: {host.address}:{host.port}
                          </div>
                        </div>
                      </div>
                      <button
                        id={`btn-connect-${host.address.replace(/\./g, '-')}`}
                        className="btn btn-primary btn-sm"
                        onClick={() => handleConnect(host)}
                        disabled={connectingHost?.address === host.address}
                      >
                        {connectingHost?.address === host.address ? <div className="spinner" /> : 'Connect'}
                      </button>
                    </div>
                  ))}
                </div>
              )}

              {/* Pairing code inputs */}
              <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                <h3>Connection Pairing Code</h3>
                <input
                  id="pairing-code-input"
                  type="text"
                  placeholder="Enter 6-digit security code"
                  maxLength={6}
                  value={pairingInput}
                  onChange={e => setPairingInput(e.target.value.replace(/\D/g, ''))}
                  style={{ fontSize: '1.4rem', letterSpacing: '0.25em', textAlign: 'center', fontFamily: 'monospace' }}
                />
                {error && <p style={{ color: 'var(--danger)', fontSize: '0.8rem' }}>{error}</p>}
              </div>

              {/* Manual Direct-IP Connect */}
              <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                <h3>Manual Connect By IP</h3>
                <div style={{ display: 'flex', gap: '10px' }}>
                  <input
                    id="manual-ip-input"
                    placeholder="192.168.1.x:45101"
                    value={manualIp}
                    onChange={e => setManualIp(e.target.value)}
                  />
                  <button className="btn btn-ghost" onClick={handleManualConnect}>Establish</button>
                </div>
              </div>
            </div>
          )}

          {/* TAB 2: Active Stream Canvas Render */}
          {tab === 'stream' && streamConnected && (
            <div style={{ width: '100%', height: 'calc(100vh - 120px)', background: '#000', position: 'relative', overflow: 'hidden' }}>
              
              {/* HTML5 Video Decoder Canvas */}
              <canvas
                ref={canvasRef}
                id="stream-canvas"
                style={{ width: '100%', height: '100%', objectFit: 'contain', display: 'block', cursor: 'none' }}
                onMouseMove={handleMouseMove}
                onMouseDown={(e) => handleMouseButton(e, true)}
                onMouseUp={(e) => handleMouseButton(e, false)}
                onWheel={handleWheel}
                onContextMenu={(e) => e.preventDefault()}
              />

              {/* Floating Overlay HUD telemetry bar */}
              <div style={{
                position: 'absolute', top: '16px', right: '16px', left: '16px',
                display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                pointerEvents: 'none', zIndex: 10
              }}>
                {/* HUD stats */}
                <div style={{
                  background: 'rgba(10, 8, 20, 0.7)', borderRadius: 'var(--radius-md)',
                  border: '1px solid var(--border)', padding: '8px 16px',
                  display: 'flex', gap: '16px', alignItems: 'center',
                  fontSize: '0.78rem', color: '#fff', backdropFilter: 'var(--glass-blur)',
                }}>
                  <Activity size={14} style={{ color: '#10b981' }} />
                  <span style={{ fontFamily: 'monospace' }}>FPS: {decodeStats.fps}</span>
                  <span style={{ fontFamily: 'monospace' }}>RTT: {sessionStats.latency_ms}ms</span>
                  <span style={{ fontFamily: 'monospace' }}>Bitrate: {(sessionStats.bitrate_kbps / 1000).toFixed(1)} Mbps</span>
                  <span style={{ fontFamily: 'monospace' }}>IDR: {decodeStats.keyframes}</span>
                </div>

                {/* HUD controls (clickable) */}
                <div style={{
                  display: 'flex', gap: '10px', pointerEvents: 'auto'
                }}>
                  <button 
                    className="btn btn-ghost btn-sm"
                    onClick={() => invoke('request_keyframe').then(() => addToast('Keyframe Requested', 'Sent IDR request to host.', 'info')).catch(console.error)}
                    style={{ background: 'rgba(10, 8, 20, 0.7)', backdropFilter: 'var(--glass-blur)' }}
                  >
                    Request IDR
                  </button>
                  
                  <button 
                    className="btn btn-ghost btn-sm"
                    onClick={toggleFullscreen}
                    style={{ background: 'rgba(10, 8, 20, 0.7)', backdropFilter: 'var(--glass-blur)' }}
                  >
                    {fullscreen ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
                  </button>
                  
                  <button 
                    className="btn btn-danger btn-sm"
                    onClick={handleDisconnect}
                  >
                    Disconnect
                  </button>
                </div>
              </div>

              {/* Decoder errors */}
              {decodeError && (
                <div style={{
                  position: 'absolute', bottom: '24px', left: '50%', transform: 'translateX(-50%)',
                  background: 'rgba(239, 68, 68, 0.95)', border: '1px solid rgba(239, 68, 68, 0.4)',
                  borderRadius: 'var(--radius-md)', padding: '10px 20px', color: '#fff', fontSize: '0.8rem',
                  backdropFilter: 'var(--glass-blur)', zIndex: 12
                }}>
                  ⚠ {decodeError}
                </div>
              )}
            </div>
          )}

          {/* TAB 3: Performance Monitor */}
          {tab === 'performance' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '20px' }}>
              {streamConnected ? (
                <>
                  {/* Dashboard stats cards */}
                  <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: '14px' }}>
                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Decode Frame Rate</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: 'var(--success)', marginTop: '6px' }}>
                        {decodeStats.fps} <span style={{ fontSize: '0.9rem' }}>FPS</span>
                      </div>
                    </div>
                    
                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Decode Delay</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: decodeStats.decodeMs > 10 ? 'var(--warning)' : 'var(--text-primary)', marginTop: '6px' }}>
                        {decodeStats.decodeMs} <span style={{ fontSize: '0.9rem' }}>ms</span>
                      </div>
                    </div>

                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Network RTT</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: sessionStats.latency_ms > 50 ? 'var(--danger)' : 'var(--accent)', marginTop: '6px' }}>
                        {sessionStats.latency_ms} <span style={{ fontSize: '0.9rem' }}>ms</span>
                      </div>
                    </div>

                    <div className="card" style={{ padding: '16px', textAlign: 'center' }}>
                      <span style={{ fontSize: '0.75rem', color: 'var(--text-secondary)' }}>Total Decoded Frames</span>
                      <div style={{ fontSize: '1.8rem', fontWeight: 700, color: 'var(--accent-purple)', marginTop: '6px' }}>
                        {decodeStats.frames}
                      </div>
                    </div>
                  </div>

                  {/* Sparkline canvas graph */}
                  <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                    <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                      <h3>Real-Time Decoding Graphs</h3>
                      <div style={{ display: 'flex', gap: '14px', fontSize: '0.75rem' }}>
                        <span style={{ color: '#10b981', display: 'flex', alignItems: 'center', gap: '4px' }}>■ Decode FPS</span>
                        <span style={{ color: '#818cf8', display: 'flex', alignItems: 'center', gap: '4px' }}>■ Decode Latency</span>
                        <span style={{ color: '#3b82f6', display: 'flex', alignItems: 'center', gap: '4px' }}>■ Network RTT</span>
                      </div>
                    </div>
                    <canvas ref={chartCanvasRef} width={600} height={180} style={{ width: '100%', height: '180px', display: 'block', background: 'rgba(0,0,0,0.15)', borderRadius: 'var(--radius-md)' }} />
                  </div>
                </>
              ) : (
                <div className="empty-state">
                  <Activity size={32} style={{ opacity: 0.3 }} />
                  <p>Performance analytics will plot once an active stream connects.</p>
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
                  <div style={{ color: 'var(--text-muted)', textAlign: 'center', padding: '20px' }}>Loading client logs...</div>
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
    </div>
  );
};
