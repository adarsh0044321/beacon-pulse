import React, { useEffect, useRef, useState, useCallback } from 'react';
import { ArrowLeft, RefreshCw, Wifi, WifiOff, Maximize2, Minimize2, Activity, Settings as SettingsIcon } from 'lucide-react';
import type { Page } from '../App';
import { useSessionStore, type DiscoveredHost } from '../store/sessionStore';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface ClientProps { onNavigate: (p: Page) => void; }

// ─────────────────────────────────────────────────────────────────────────────
// WebCodecs decoder hook
// ─────────────────────────────────────────────────────────────────────────────

interface DecoderStats {
  fps: number;
  decodeMs: number;
  frames: number;
  keyframes: number;
}

function useWebCodecsDecoder(canvasRef: React.RefObject<HTMLCanvasElement>) {
  const decoderRef = useRef<VideoDecoder | null>(null);
  const pendingConfigRef = useRef<{ width: number; height: number } | null>(null);
  const frameCountRef = useRef(0);
  const lastFpsCheck = useRef(Date.now());
  const [stats, setStats] = useState<DecoderStats>({ fps: 0, decodeMs: 0, frames: 0, keyframes: 0 });
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);

  const initDecoder = useCallback((width: number, height: number) => {
    // Tear down any existing decoder first
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

        // FPS measurement every second
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
      codec: 'avc1.42E01E',           // Baseline H.264
      codedWidth: width,
      codedHeight: height,
      optimizeForLatency: true,        // Critical for streaming
      hardwareAcceleration: 'prefer-hardware',
    });

    decoderRef.current = decoder;
    setReady(true);
    setError(null);
    console.log(`[WebCodecs] Decoder configured ${width}×${height}`);
  }, [canvasRef]);

  const feedChunk = useCallback((
    data: string,        // base64 Annex-B NAL bytes
    timestampUs: number,
    isKeyframe: boolean,
    width: number,
    height: number,
  ) => {
    // Lazy-init or reinit on dimension change
    if (!decoderRef.current || decoderRef.current.state === 'closed') {
      initDecoder(width, height);
    }
    const decoder = decoderRef.current!;
    if (decoder.state !== 'configured') return;

    // Decode base64 → Uint8Array
    const raw = atob(data);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);

    const chunk = new EncodedVideoChunk({
      type: isKeyframe ? 'key' : 'delta',
      timestamp: timestampUs,
      data: bytes,
    });

    try {
      decoder.decode(chunk);
      if (isKeyframe) setStats(prev => ({ ...prev, keyframes: prev.keyframes + 1 }));
    } catch (e) {
      console.warn('[WebCodecs] decode() threw:', e);
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
// IPC bridge (Tauri invoke + event listener)
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// Main component
// ─────────────────────────────────────────────────────────────────────────────

export const Client: React.FC<ClientProps> = ({ onNavigate }) => {
  const {
    discoveredHosts, hostsLoading, discoverHosts,
    connectedHost, connectToHost, disconnectFromHost,
    stats: sessionStats,
  } = useSessionStore();

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const { feedChunk, closeDecoder, stats: decodeStats, error: decodeError, ready } =
    useWebCodecsDecoder(canvasRef as React.RefObject<HTMLCanvasElement>);

  const [pairingInput, setPairingInput] = useState('');
  const [connectingHost, setConnectingHost] = useState<DiscoveredHost | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [manualIp, setManualIp] = useState('');
  const [recvPort] = useState(45102); // local port for receiving stream
  const [streamConnected, setStreamConnected] = useState(false);

  // ── Auto-discover hosts on mount + periodic re-scan ─────────────────────
  useEffect(() => {
    let cancelled = false;
    let intervalId: ReturnType<typeof setInterval> | null = null;

    const runDiscovery = async () => {
      if (cancelled) return;
      try {
        await discoverHosts();
      } catch {
        // IPC not ready yet — will retry
      }
    };

    // Initial discovery: retry a few times with delays (service may be starting)
    const initialScan = async () => {
      for (let i = 0; i < 6 && !cancelled; i++) {
        await runDiscovery();
        // Check if we found hosts
        const { discoveredHosts: hosts } = useSessionStore.getState();
        if (hosts.length > 0) break;
        // Wait before retry
        await new Promise(r => setTimeout(r, 2000));
      }
      // Start periodic re-scan every 8 seconds for late-joining devices
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

  // ── Listen for IPC events from the service ────────────────────────────────
  useEffect(() => {
    const unlisten = listen<string>('service-event', (event) => {
      try {
        const ev = typeof event.payload === 'string'
          ? JSON.parse(event.payload)
          : event.payload;
        handleServiceEvent(ev);
      } catch (e) {
        console.error('[IPC] Event parse error:', e);
      }
    });
    return () => { unlisten.then(f => f()); };
  }, [feedChunk]);

  const handleServiceEvent = useCallback((ev: any) => {
    switch (ev.event) {
      case 'video_chunk':
        feedChunk(ev.data, ev.timestamp_us, ev.is_keyframe, ev.width, ev.height);
        break;
      case 'stream_connected':
        setStreamConnected(true);
        setError(null);
        break;
      case 'stream_disconnected':
        setStreamConnected(false);
        closeDecoder();
        break;
      case 'error':
        setError(ev.message);
        break;
    }
  }, [feedChunk, closeDecoder]);

  // ── Input Event Handlers ──────────────────────────────────────────────────
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
    if (e.button === 0) btn = 0; // Left
    else if (e.button === 2) btn = 1; // Right
    else if (e.button === 1) btn = 2; // Middle
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

  // Keyboard hooks when stream is connected
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

  // ── Toggle fullscreen ─────────────────────────────────────────────────────
  const toggleFullscreen = () => {
    const el = canvasRef.current?.parentElement;
    if (!el) return;
    if (!document.fullscreenElement) {
      el.requestFullscreen().then(() => setFullscreen(true));
    } else {
      document.exitFullscreen().then(() => setFullscreen(false));
    }
  };

  // ── Connect flow ──────────────────────────────────────────────────────────
  const handleConnect = async (host: DiscoveredHost, skipPairingCheck = false) => {
    if (!skipPairingCheck && pairingInput && pairingInput.length !== 6) {
      setError('Enter the 6-digit pairing code from the host (or leave blank for direct connect)');
      return;
    }
    setError(null);
    setConnectingHost(host);

    try {
      // connectToHost invokes the Tauri command which sends join_stream to the service.
      // The service does the full TCP handshake + optional pairing + UDP recv setup.
      await connectToHost(host, pairingInput);
    } catch {
      setError('Connection failed. Check the code and try again.');
    }
    setConnectingHost(null);
  };

  const handleDisconnect = async () => {
    closeDecoder();
    setStreamConnected(false);
    disconnectFromHost();
  };

  const handleManualConnect = () => {
    if (!manualIp) return;
    const [ip, portStr] = manualIp.split(':');
    // Manual connections bypass pairing — pass empty code (service auto-accepts)
    const port = parseInt(portStr || '45101');
    handleConnect({ name: ip, address: ip, port }, true);
  };

  // ─────────────────────────────────────────────────────────────────────────
  return (
    <div className="page">
      <div className="page-header">
        {connectedHost ? (
          <button className="btn btn-ghost btn-sm" onClick={handleDisconnect}>
            <ArrowLeft size={14} /> Disconnect
          </button>
        ) : import.meta.env.MODE === 'player' ? (
          <button className="btn btn-ghost btn-sm" onClick={() => onNavigate('settings')}>
            <SettingsIcon size={14} /> Settings
          </button>
        ) : (
          <button className="btn btn-ghost btn-sm" onClick={() => onNavigate('home')}>
            <ArrowLeft size={14} /> Back
          </button>
        )}
        <h2 style={{ flex: 1 }}>{connectedHost ? 'Viewing Stream' : 'Join Stream'}</h2>
        {connectedHost && (
          <button className="btn btn-ghost btn-sm" onClick={toggleFullscreen}>
            {fullscreen ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
          </button>
        )}
      </div>

      {connectedHost ? (
        /* ── Stream Viewer ────────────────────────────────────────────── */
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', background: '#000', position: 'relative' }}>

          {/* WebCodecs canvas — actual decoded frames */}
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

          {/* Connecting overlay */}
          {!streamConnected && (
            <div style={{
              position: 'absolute', inset: 0,
              display: 'flex', flexDirection: 'column',
              alignItems: 'center', justifyContent: 'center',
              background: 'rgba(0,0,0,0.85)', gap: '12px',
            }}>
              <div className="spinner" style={{ width: '32px', height: '32px' }} />
              <p style={{ color: '#aaa', fontSize: '0.875rem' }}>Connecting to stream…</p>
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => {
                  invoke('request_keyframe').catch(console.error);
                }}
              >
                Request Keyframe
              </button>
            </div>
          )}

          {/* Decode error overlay */}
          {decodeError && (
            <div style={{
              position: 'absolute', bottom: '40px', left: '50%', transform: 'translateX(-50%)',
              background: 'rgba(220,38,38,0.85)', borderRadius: '6px',
              padding: '8px 16px', color: '#fff', fontSize: '0.75rem',
              backdropFilter: 'blur(8px)',
            }}>
              {decodeError}
            </div>
          )}

          {/* HUD: live stats overlay */}
          <div style={{
            position: 'absolute', top: '8px', right: '8px',
            background: 'rgba(0,0,0,0.65)', borderRadius: '8px',
            padding: '6px 12px', fontSize: '0.72rem', color: '#ccc',
            backdropFilter: 'blur(10px)',
            display: 'flex', gap: '12px', alignItems: 'center',
          }}>
            <Activity size={11} style={{ color: streamConnected ? '#22c55e' : '#ef4444' }} />
            <span style={{ fontVariantNumeric: 'tabular-nums' }}>
              {decodeStats.fps} fps
            </span>
            <span>{sessionStats.latency_ms}ms</span>
            <span>{(sessionStats.bitrate_kbps / 1000).toFixed(1)} Mbps</span>
            <span style={{ color: '#666' }}>·</span>
            <span>{decodeStats.keyframes} IDR</span>
          </div>
        </div>

      ) : (
        /* ── Discovery + Connect ──────────────────────────────────────── */
        <div className="page-content">

          {/* Host list */}
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
            <h3>Available Hosts</h3>
            <button className="btn btn-ghost btn-sm" onClick={discoverHosts} disabled={hostsLoading}>
              <RefreshCw size={12} />
              {hostsLoading ? 'Scanning…' : 'Refresh'}
            </button>
          </div>

          {discoveredHosts.length === 0 && hostsLoading ? (
            <div className="empty-state"><div className="spinner" /><p>Scanning LAN…</p></div>
          ) : discoveredHosts.length === 0 ? (
            <div className="empty-state">
              <WifiOff size={32} style={{ opacity: 0.3 }} />
              <p>No hosts found on this network</p>
              <span style={{ fontSize: '0.75rem', color: 'var(--text-muted)' }}>
                Make sure the host has LANShare running
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
              {discoveredHosts.map(host => (
                <div key={host.address} className="device-card">
                  <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
                    <Wifi size={18} style={{ color: 'var(--accent)' }} />
                    <div>
                      <div style={{ fontWeight: 500, fontSize: '0.875rem' }}>{host.name}</div>
                      <div style={{ fontSize: '0.75rem', color: 'var(--text-muted)' }}>
                        {host.address}:{host.port}
                      </div>
                    </div>
                  </div>
                  <button
                    id={`btn-connect-${host.address.replace(/\./g, '-')}`}
                    className="btn btn-primary btn-sm"
                    onClick={() => handleConnect(host)}
                    disabled={connectingHost?.address === host.address}
                  >
                    {connectingHost?.address === host.address
                      ? <div className="spinner" />
                      : 'Connect'}
                  </button>
                </div>
              ))}
            </div>
          )}

          {/* Pairing code */}
          <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
            <h3>Pairing Code</h3>
            <input
              id="pairing-code-input"
              type="text"
              placeholder="6-digit code from host"
              maxLength={6}
              value={pairingInput}
              onChange={e => setPairingInput(e.target.value.replace(/\D/g, ''))}
              style={{ fontSize: '1.2rem', letterSpacing: '0.2em', textAlign: 'center' }}
            />
            {error && <p style={{ color: 'var(--danger)', fontSize: '0.8rem' }}>{error}</p>}
          </div>

          {/* Manual IP */}
          <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
            <h3>Manual Connection</h3>
            <div style={{ display: 'flex', gap: '8px' }}>
              <input
                id="manual-ip-input"
                placeholder="192.168.1.x  (or 192.168.1.x:45101)"
                value={manualIp}
                onChange={e => setManualIp(e.target.value)}
              />
              <button className="btn btn-ghost" onClick={handleManualConnect}>Connect</button>
            </div>
          </div>
        </div>
      )}

      {/* Bottom stats bar */}
      {connectedHost && (
        <div className="stats-bar">
          <div className="stat-item">FPS <span className="stat-value">{decodeStats.fps}</span></div>
          <div className="stat-item">Latency <span className="stat-value">{sessionStats.latency_ms}ms</span></div>
          <div className="stat-item">Bitrate <span className="stat-value">{(sessionStats.bitrate_kbps / 1000).toFixed(1)} Mbps</span></div>
          <div className="stat-item">Frames <span className="stat-value">{decodeStats.frames}</span></div>
          <div className="stat-item">IDR <span className="stat-value">{decodeStats.keyframes}</span></div>
        </div>
      )}
    </div>
  );
};
