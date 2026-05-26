import React, { useCallback } from 'react';
import { ArrowLeft, Save, Cpu, Zap, Monitor } from 'lucide-react';
import type { Page } from '../App';
import { useSettingsStore, type EncoderType, type IndicatorMode } from '../store/settingsStore';
import { useSessionStore } from '../store/sessionStore';

interface SettingsProps { onNavigate: (p: Page) => void; }

// ─────────────────────────────────────────────────────────────────────────────
// Reusable components
// ─────────────────────────────────────────────────────────────────────────────

interface ToggleRowProps {
  label: string;
  description?: string;
  checked: boolean;
  onChange: () => void;
  id: string;
}

const ToggleRow: React.FC<ToggleRowProps> = ({ label, description, checked, onChange, id }) => (
  <div className="toggle-row">
    <div className="toggle-label">
      <span>{label}</span>
      {description && <small>{description}</small>}
    </div>
    <label className="toggle" htmlFor={id}>
      <input id={id} type="checkbox" checked={checked} onChange={onChange} />
      <div className="toggle-track" />
      <div className="toggle-thumb" />
    </label>
  </div>
);

// ─────────────────────────────────────────────────────────────────────────────
// Encoder status badge (Phase 3)
// ─────────────────────────────────────────────────────────────────────────────

interface EncoderBadgeProps {
  encoderName: string;
  vendor: string;
  hwAccelerated: boolean;
}

const vendorColor: Record<string, string> = {
  NVIDIA: '#76b900',
  AMD:    '#ed1c24',
  Intel:  '#0071c5',
  Software: '#888',
};

const EncoderBadge: React.FC<EncoderBadgeProps> = ({ encoderName, vendor, hwAccelerated }) => {
  const color = vendorColor[vendor] ?? '#888';
  return (
    <div
      id="encoder-badge"
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '10px',
        padding: '10px 14px',
        borderRadius: '10px',
        background: 'var(--bg-secondary)',
        border: `1px solid ${color}44`,
      }}
    >
      {hwAccelerated
        ? <Zap size={16} style={{ color, flexShrink: 0 }} />
        : <Cpu  size={16} style={{ color, flexShrink: 0 }} />}
      <div style={{ flex: 1 }}>
        <div style={{ fontSize: '0.875rem', fontWeight: 600, color: 'var(--text-primary)' }}>
          {encoderName}
          {hwAccelerated && (
            <span style={{
              marginLeft: '8px', fontSize: '0.7rem', padding: '2px 6px',
              borderRadius: '4px', background: `${color}22`, color,
            }}>
              HW
            </span>
          )}
        </div>
        <div style={{ fontSize: '0.75rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
          {vendor} {hwAccelerated ? '· Hardware accelerated' : '· Software encoding'}
        </div>
      </div>
      <Monitor size={13} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />
    </div>
  );
};

// ─────────────────────────────────────────────────────────────────────────────
// GPU zero-copy path badge (Phase 5)
// ─────────────────────────────────────────────────────────────────────────────

const GpuPathBadge: React.FC<{ active: boolean }> = ({ active }) => (
  <div
    id="gpu-path-badge"
    style={{
      display: 'flex',
      alignItems: 'center',
      gap: '10px',
      padding: '10px 14px',
      borderRadius: '10px',
      background: 'var(--bg-secondary)',
      border: `1px solid ${active ? '#4ade8044' : '#f59e0b44'}`,
    }}
  >
    {active
      ? <Zap size={16} style={{ color: '#4ade80', flexShrink: 0 }} />
      : <Cpu size={16} style={{ color: '#f59e0b', flexShrink: 0 }} />}
    <div style={{ flex: 1 }}>
      <div style={{ fontSize: '0.875rem', fontWeight: 600, color: 'var(--text-primary)' }}>
        Zero-Copy GPU Path
        <span style={{
          marginLeft: '8px', fontSize: '0.7rem', padding: '2px 6px',
          borderRadius: '4px',
          background: active ? '#4ade8022' : '#f59e0b22',
          color:      active ? '#4ade80'   : '#f59e0b',
        }}>
          {active ? 'ACTIVE' : 'CPU FALLBACK'}
        </span>
      </div>
      <div style={{ fontSize: '0.75rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
        {active
          ? 'Frames travel GPU → Encoder with no CPU copy'
          : 'GPU texture path inactive — encoding from CPU BGRA buffer'}
      </div>
    </div>
  </div>
);

// ─────────────────────────────────────────────────────────────────────────────
// Settings page
// ─────────────────────────────────────────────────────────────────────────────

export const Settings: React.FC<SettingsProps> = ({ onNavigate }) => {
  const s    = useSettingsStore();
  const sess = useSessionStore();

  // Send bitrate to service in real-time when slider moves (debounce via requestIdleCallback)
  const handleBitrateChange = useCallback((kbps: number) => {
    s.setBitrate(kbps);
    if (sess.isSharing) {
      sess.setBitrate(kbps);
    }
  }, [s, sess]);

  return (
    <div className="page">
      <div className="page-header">
        <button
          className="btn btn-ghost btn-sm"
          onClick={() => {
            const mode = import.meta.env.MODE;
            if (mode === 'host') onNavigate('host');
            else if (mode === 'player') onNavigate('client');
            else onNavigate('home');
          }}
        >
          <ArrowLeft size={14} /> Back
        </button>
        <h2 style={{ flex: 1 }}>Settings</h2>
        <button id="btn-save-settings" className="btn btn-primary btn-sm" onClick={() => s.save()}>
          <Save size={14} /> Save
        </button>
      </div>

      <div className="page-content">

        {/* Stream Quality */}
        <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
          <h3>Stream Quality</h3>

          {/* Phase 3: Active encoder badge */}
          {sess.encoderInfo && (
            <EncoderBadge
              encoderName={sess.encoderInfo.encoder_name}
              vendor={sess.encoderInfo.vendor}
              hwAccelerated={sess.encoderInfo.hw_accelerated}
            />
          )}

          {/* Phase 5: GPU zero-copy path status */}
          {sess.isSharing && (
            <GpuPathBadge active={sess.stats.gpu_path_active} />
          )}

          <div>
            <label style={{ fontSize: '0.8rem', color: 'var(--text-secondary)', display: 'block', marginBottom: '6px' }}>
              Bitrate: <strong style={{ color: 'var(--text-primary)' }}>{s.bitrate_kbps} kbps</strong>
              {sess.isSharing && (
                <span style={{ marginLeft: '8px', fontSize: '0.7rem', color: 'var(--accent)' }}>
                  · live
                </span>
              )}
            </label>
            <input
              id="bitrate-slider"
              type="range"
              min={1000} max={20000} step={500}
              value={s.bitrate_kbps}
              onChange={e => handleBitrateChange(Number(e.target.value))}
              style={{ padding: 0, cursor: 'pointer' }}
            />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', color: 'var(--text-tertiary)', marginTop: '4px' }}>
              <span>1 Mbps</span>
              <span>10 Mbps</span>
              <span>20 Mbps</span>
            </div>
          </div>

          <div>
            <label style={{ fontSize: '0.8rem', color: 'var(--text-secondary)', display: 'block', marginBottom: '6px' }}>
              Frame Rate: <strong style={{ color: 'var(--text-primary)' }}>{s.fps} FPS</strong>
            </label>
            <div style={{ display: 'flex', gap: '8px' }}>
              {[30, 60].map(fps => (
                <button
                  key={fps}
                  id={`fps-${fps}`}
                  className={`btn btn-sm ${s.fps === fps ? 'btn-primary' : 'btn-ghost'}`}
                  onClick={() => s.setFps(fps)}
                >
                  {fps} FPS
                </button>
              ))}
            </div>
          </div>

          <div>
            <label style={{ fontSize: '0.8rem', color: 'var(--text-secondary)', display: 'block', marginBottom: '6px' }}>
              Encoder Preference
            </label>
            <select
              id="encoder-select"
              value={s.encoder}
              onChange={e => s.setEncoder(e.target.value as EncoderType)}
            >
              <option value="auto">Auto (best available)</option>
              <option value="nvenc">NVENC — NVIDIA GPU</option>
              <option value="amf">AMF — AMD GPU</option>
              <option value="qsv">QuickSync — Intel GPU</option>
              <option value="software">Software (OpenH264) — Compatible</option>
            </select>
            <small style={{ display: 'block', marginTop: '6px', color: 'var(--text-tertiary)' }}>
              Active encoder is shown above when sharing is active.
            </small>
          </div>
        </div>

        {/* Permissions */}
        <div className="card">
          <h3 style={{ marginBottom: '8px' }}>Permissions</h3>
          <ToggleRow id="toggle-input"     label="Allow Remote Input"   description="Client can control mouse and keyboard" checked={s.allow_input_control}  onChange={s.toggleInputControl} />
          <ToggleRow id="toggle-audio"     label="Share Audio"          description="Stream system audio to client"         checked={s.audio_enabled}        onChange={s.toggleAudio} />
          <ToggleRow id="toggle-clipboard" label="Clipboard Sharing"    description="Sync clipboard between host and client" checked={s.clipboard_enabled}   onChange={s.toggleClipboard} />
        </div>

        {/* System */}
        <div className="card">
          <h3 style={{ marginBottom: '8px' }}>System</h3>
          <ToggleRow id="toggle-startup"    label="Start with Windows"  description="Launch service automatically on boot"    checked={s.start_with_windows} onChange={s.toggleStartWithWindows} />
          <ToggleRow id="toggle-unattended" label="Unattended Mode"     description="Allow connections when no user is logged in" checked={s.unattended_mode} onChange={() => {}} />
        </div>

        {/* Sharing Indicator */}
        <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
          <h3>Sharing Indicator</h3>
          <p style={{ fontSize: '0.8rem' }}>Controls the tray icon visibility when sharing is active</p>
          {(['always_show', 'hide_session', 'always_hide'] as IndicatorMode[]).map(mode => (
            <label
              key={mode}
              id={`indicator-${mode}`}
              style={{ display: 'flex', alignItems: 'center', gap: '10px', cursor: 'pointer', fontSize: '0.875rem' }}
            >
              <input
                type="radio"
                name="indicator-mode"
                checked={s.indicator_mode === mode}
                onChange={() => s.setIndicatorMode(mode)}
                style={{ width: 'auto' }}
              />
              {{
                always_show:  'Always show indicator (recommended)',
                hide_session: 'Hide for this session only',
                always_hide:  'Always hide indicator',
              }[mode]}
            </label>
          ))}
        </div>

      </div>
    </div>
  );
};
