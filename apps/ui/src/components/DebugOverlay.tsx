import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import './DebugOverlay.css'

interface MetricsSnapshot {
  frames_captured: number
  frames_encoded: number
  frames_dropped_cap: number
  frames_stale: number
  backend_switches: number
  keyframes: number
  avg_encode_us: number
  avg_pipeline_us: number
  bytes_in_window: number
  packets_in_window: number
  packet_loss_in_window: number
  rtt_us: number
  render_suspended_count: number
  frame_width: number
  frame_height: number
}

interface PerformanceWarning {
  kind: 'PacketLoss' | 'HighLatency' | 'SlowEncoder' | 'FrameDrops' | 'RenderSuspended'
  value?: number
}

interface DebugOverlayProps {
  backend: string
  sessionId?: string
}

const HISTORY_LEN = 60 // 30 seconds at 500ms

export function DebugOverlay({ backend, sessionId }: DebugOverlayProps) {
  const [visible, setVisible] = useState(false)
  const [metrics, setMetrics] = useState<MetricsSnapshot | null>(null)
  const [warnings, setWarnings] = useState<PerformanceWarning[]>([])
  const [history, setHistory] = useState<MetricsSnapshot[]>([])
  const canvasRef = useRef<HTMLCanvasElement>(null)

  // Ctrl+Shift+D toggle
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key === 'D') {
        setVisible(v => !v)
        e.preventDefault()
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  // Listen for metrics events from Tauri backend
  useEffect(() => {
    const unlisten = listen<MetricsSnapshot>('metrics_update', (event) => {
      const snap = event.payload
      setMetrics(snap)
      setHistory(h => {
        const next = [...h, snap]
        return next.length > HISTORY_LEN ? next.slice(-HISTORY_LEN) : next
      })
      // Compute warnings
      const w: PerformanceWarning[] = []
      const lossPct = snap.packets_in_window > 0
        ? (snap.packet_loss_in_window / (snap.packets_in_window + snap.packet_loss_in_window)) * 100
        : 0
      if (lossPct > 5) w.push({ kind: 'PacketLoss', value: lossPct })
      if (snap.rtt_us / 1000 > 100) w.push({ kind: 'HighLatency', value: snap.rtt_us / 1000 })
      if (snap.avg_encode_us / 1000 > 20) w.push({ kind: 'SlowEncoder', value: snap.avg_encode_us / 1000 })
      if (snap.frames_dropped_cap > 10) w.push({ kind: 'FrameDrops', value: snap.frames_dropped_cap })
      setWarnings(w)
    })
    return () => { unlisten.then(f => f()) }
  }, [])

  // Draw mini sparkline chart
  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas || history.length < 2) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    const w = canvas.width, h = canvas.height
    ctx.clearRect(0, 0, w, h)

    // Pipeline latency sparkline (green)
    const vals = history.map(s => s.avg_pipeline_us / 1000) // ms
    const max = Math.max(...vals, 20)
    ctx.beginPath()
    ctx.strokeStyle = '#00ff88'
    ctx.lineWidth = 1.5
    vals.forEach((v, i) => {
      const x = (i / (vals.length - 1)) * w
      const y = h - (v / max) * h
      i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)
    })
    ctx.stroke()

    // 16ms target line (60fps threshold)
    ctx.beginPath()
    ctx.strokeStyle = 'rgba(255,200,0,0.4)'
    ctx.setLineDash([3, 3])
    const targetY = h - (16 / max) * h
    ctx.moveTo(0, targetY)
    ctx.lineTo(w, targetY)
    ctx.stroke()
    ctx.setLineDash([])
  }, [history])

  if (!visible) {
    return (
      <div className="debug-hint" title="Ctrl+Shift+D to open debug overlay">
        <span className="debug-hint-icon">⚙</span>
      </div>
    )
  }

  const enc_ms = metrics ? (metrics.avg_encode_us / 1000).toFixed(1) : '—'
  const pipe_ms = metrics ? (metrics.avg_pipeline_us / 1000).toFixed(1) : '—'
  const rtt_ms = metrics ? (metrics.rtt_us / 1000).toFixed(1) : '—'
  const bitrate = metrics
    ? ((metrics.bytes_in_window * 8 * 2) / 1_000_000).toFixed(2)
    : '—'
  const lossPct = metrics && metrics.packets_in_window > 0
    ? ((metrics.packet_loss_in_window / (metrics.packets_in_window + metrics.packet_loss_in_window)) * 100).toFixed(1)
    : '0.0'

  return (
    <div className="debug-overlay" id="debug-overlay">
      <div className="debug-header">
        <span className="debug-title">⚙ Debug Overlay</span>
        <span className="debug-session">{sessionId}</span>
        <button className="debug-close" onClick={() => setVisible(false)}>✕</button>
      </div>

      <div className="debug-grid">
        <div className="debug-section">
          <div className="debug-label">Backend</div>
          <div className={`debug-value backend-badge backend-${backend.toLowerCase()}`}>{backend}</div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Resolution</div>
          <div className="debug-value">
            {metrics ? `${metrics.frame_width}×${metrics.frame_height}` : '—'}
          </div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Encode</div>
          <div className={`debug-value ${parseFloat(enc_ms) > 16 ? 'debug-warn' : 'debug-ok'}`}>
            {enc_ms} ms
          </div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Pipeline</div>
          <div className={`debug-value ${parseFloat(pipe_ms) > 30 ? 'debug-warn' : 'debug-ok'}`}>
            {pipe_ms} ms
          </div>
        </div>
        <div className="debug-section">
          <div className="debug-label">RTT</div>
          <div className={`debug-value ${parseFloat(rtt_ms) > 100 ? 'debug-warn' : 'debug-ok'}`}>
            {rtt_ms} ms
          </div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Bitrate</div>
          <div className="debug-value">{bitrate} Mbps</div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Pkt Loss</div>
          <div className={`debug-value ${parseFloat(lossPct) > 1 ? 'debug-warn' : 'debug-ok'}`}>
            {lossPct}%
          </div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Dropped</div>
          <div className={`debug-value ${(metrics?.frames_dropped_cap ?? 0) > 5 ? 'debug-warn' : 'debug-ok'}`}>
            {metrics?.frames_dropped_cap ?? 0}
          </div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Stale</div>
          <div className="debug-value">{metrics?.frames_stale ?? 0}</div>
        </div>
        <div className="debug-section">
          <div className="debug-label">Switches</div>
          <div className="debug-value">{metrics?.backend_switches ?? 0}</div>
        </div>
      </div>

      {/* Pipeline latency sparkline */}
      <div className="debug-chart-label">Pipeline latency (30s) — 16ms target ▼</div>
      <canvas ref={canvasRef} className="debug-chart" width={280} height={48} />

      {/* Active warnings */}
      {warnings.length > 0 && (
        <div className="debug-warnings">
          {warnings.map((w, i) => (
            <div key={i} className="debug-warning-item">
              ⚠ {formatWarning(w)}
            </div>
          ))}
        </div>
      )}

      <div className="debug-footer">Ctrl+Shift+D to close</div>
    </div>
  )
}

function formatWarning(w: PerformanceWarning): string {
  switch (w.kind) {
    case 'PacketLoss': return `Packet loss ${w.value?.toFixed(1)}% (threshold: 5%)`
    case 'HighLatency': return `High RTT ${w.value?.toFixed(0)}ms (threshold: 100ms)`
    case 'SlowEncoder': return `Slow encoder ${w.value?.toFixed(1)}ms (threshold: 20ms)`
    case 'FrameDrops': return `Frame drops: ${w.value} frames`
    case 'RenderSuspended': return 'App render suspended — serving stale frames'
  }
}
