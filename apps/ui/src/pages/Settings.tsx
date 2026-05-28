import React, { useCallback } from 'react';
import { ArrowLeft, Save, Cpu, Zap, Monitor } from 'lucide-react';
import type { Page } from '../App';
import { useSettingsStore, type EncoderType, type IndicatorMode } from '../store/settingsStore';
import { useSessionStore } from '../store/sessionStore';
import { useToastStore } from '../store/toastStore';

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
// Encoder status badge
// ─────────────────────────────────────────────────────────────────────────────

interface EncoderBadgeProps {
  encoderName: string;
  vendor: string;
  hwAccelerated: boolean;
}

const vendorColor: Record<string, string> = {
  NVIDIA: '#10b981',   // Neon Emerald
  AMD:    '#ef4444',   // Rose
  Intel:  '#3b82f6',   // Cyan
  Software: '#64748b', // Slate
};

const EncoderBadge: React.FC<EncoderBadgeProps> = ({ encoderName, vendor, hwAccelerated }) => {
  const color = vendorColor[vendor] ?? '#64748b';
  return (
    <div
      id="encoder-badge"
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '12px',
        padding: '12px 16px',
        borderRadius: '12px',
        background: 'rgba(8, 7, 15, 0.4)',
        border: `1px solid ${color}40`,
        boxShadow: `0 0 10px ${color}15`,
      }}
    >
      {hwAccelerated
        ? <Zap size={18} style={{ color, filter: `drop-shadow(0 0 5px ${color})` }} />
        : <Cpu  size={18} style={{ color }} />}
      <div style={{ flex: 1 }}>
        <div style={{ fontSize: '0.9rem', fontWeight: 650, color: 'var(--text-primary)' }}>
          {encoderName}
          {hwAccelerated && (
            <span style={{
              marginLeft: '8px', fontSize: '0.68rem', padding: '2px 8px',
              borderRadius: '4px', background: `${color}25`, color, fontWeight: 700
            }}>
              HW
            </span>
          )}
        </div>
        <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
          {vendor} · {hwAccelerated ? 'Hardware accelerated decoding active' : 'Software emulation mode'}
        </div>
      </div>
      <Monitor size={14} style={{ color: 'var(--text-muted)' }} />
    </div>
  );
};

// ─────────────────────────────────────────────────────────────────────────────
// GPU zero-copy path badge
// ─────────────────────────────────────────────────────────────────────────────

const GpuPathBadge: React.FC<{ active: boolean }> = ({ active }) => (
  <div
    id="gpu-path-badge"
    style={{
      display: 'flex',
      alignItems: 'center',
      gap: '12px',
      padding: '12px 16px',
      borderRadius: '12px',
      background: 'rgba(8, 7, 15, 0.4)',
      border: `1px solid ${active ? '#10b98135' : '#f59e0b35'}`,
      boxShadow: `0 0 10px ${active ? '#10b98110' : '#f59e0b10'}`,
    }}
  >
    {active
      ? <Zap size={18} style={{ color: '#10b981', filter: 'drop-shadow(0 0 5px rgba(16,185,129,0.5))' }} />
      : <Cpu size={18} style={{ color: '#f59e0b' }} />}
    <div style={{ flex: 1 }}>
      <div style={{ fontSize: '0.9rem', fontWeight: 650, color: 'var(--text-primary)' }}>
        Zero-Copy GPU Path
        <span style={{
          marginLeft: '8px', fontSize: '0.68rem', padding: '2px 8px',
          borderRadius: '4px',
          background: active ? 'rgba(16, 185, 129, 0.12)' : 'rgba(245, 158, 11, 0.12)',
          color:      active ? '#10b981'   : '#f59e0b',
          fontWeight: 700
        }}>
          {active ? 'DIRECT' : 'CPU FALLBACK'}
        </span>
      </div>
      <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
        {active
          ? 'Display frames travel GPU → Encoder with no host RAM copy overhead'
          : 'GPU texture paths inactive — copy buffer via CPU thread'}
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
  const { addToast } = useToastStore();

  const handleBitrateChange = useCallback((kbps: number) => {
    s.setBitrate(kbps);
    if (sess.isSharing) {
      sess.setBitrate(kbps);
    }
  }, [s, sess]);

  const handleSave = async () => {
    try {
      await s.save();
      addToast('Settings Saved', 'System configurations successfully written to local disk.', 'success');
    } catch (e) {
      addToast('Save Failed', 'Unable to write configurations.', 'error');
    }
  };

  return (
    <div className="page" style={{ height: '100vh', overflow: 'hidden' }}>
      <div className="page-header" style={{ display: 'flex', alignItems: 'center', gap: '16px' }}>
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
        <h2 style={{ flex: 1 }}>Settings Panel</h2>
        <button id="btn-save-settings" className="btn btn-primary btn-sm" onClick={handleSave}>
          <Save size={14} /> Save Configuration
        </button>
      </div>

      <div className="page-content" style={{ overflowY: 'auto', maxHeight: 'calc(100vh - 80px)' }}>
        
        {/* Stream Quality */}
        <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '18px' }}>
          <h3 style={{ borderBottom: '1px solid var(--border)', paddingBottom: '6px' }}>Stream Quality</h3>

          {/* Active encoder badge */}
          {sess.encoderInfo && (
            <EncoderBadge
              encoderName={sess.encoderInfo.encoder_name}
              vendor={sess.encoderInfo.vendor}
              hwAccelerated={sess.encoderInfo.hw_accelerated}
            />
          )}

          {/* GPU zero-copy path status */}
          {sess.isSharing && (
            <GpuPathBadge active={sess.stats.gpu_path_active} />
          )}

          <div>
            <label style={{ fontSize: '0.85rem', color: 'var(--text-secondary)', display: 'block', marginBottom: '8px' }}>
              Target Bitrate: <strong style={{ color: 'var(--text-primary)', fontFamily: 'monospace' }}>{(s.bitrate_kbps / 1000).toFixed(1)} Mbps</strong> ({s.bitrate_kbps} kbps)
              {sess.isSharing && (
                <span className="badge badge-active" style={{ marginLeft: '10px', fontSize: '0.65rem', padding: '1px 6px' }}>
                  Live Active
                </span>
              )}
            </label>
            <input
              id="bitrate-slider"
              type="range"
              min={1000} max={20000} step={500}
              value={s.bitrate_kbps}
              onChange={e => handleBitrateChange(Number(e.target.value))}
              style={{ width: '100%', display: 'block' }}
            />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', color: 'var(--text-muted)', marginTop: '6px' }}>
              <span>1.0 Mbps</span>
              <span>10.0 Mbps</span>
              <span>20.0 Mbps</span>
            </div>
          </div>

          <div>
            <label style={{ fontSize: '0.85rem', color: 'var(--text-secondary)', display: 'block', marginBottom: '8px' }}>
              Framerate Constraint: <strong style={{ color: 'var(--text-primary)' }}>{s.fps} FPS</strong>
            </label>
            <div style={{ display: 'flex', gap: '10px' }}>
              {[30, 60].map(fps => (
                <button
                  key={fps}
                  id={`fps-${fps}`}
                  className={`btn btn-sm ${s.fps === fps ? 'btn-primary' : 'btn-ghost'}`}
                  onClick={() => s.setFps(fps)}
                  style={{ minWidth: '80px' }}
                >
                  {fps} FPS
                </button>
              ))}
            </div>
          </div>

          <div>
            <label style={{ fontSize: '0.85rem', color: 'var(--text-secondary)', display: 'block', marginBottom: '8px' }}>
              Preferred Hardware Encoder
            </label>
            <select
              id="encoder-select"
              value={s.encoder}
              onChange={e => s.setEncoder(e.target.value as EncoderType)}
            >
              <option value="auto">Auto (Choose optimal hardware accelerator)</option>
              <option value="nvenc">NVENC — NVIDIA GPU Pipeline</option>
              <option value="amf">AMF — AMD GPU Core Pipeline</option>
              <option value="qsv">QuickSync — Intel Core Engine</option>
              <option value="software">OpenH264 — CPU Compatibility Mode</option>
            </select>
          </div>
        </div>

        {/* Permissions */}
        <div className="card">
          <h3 style={{ borderBottom: '1px solid var(--border)', paddingBottom: '6px', marginBottom: '10px' }}>Host Access Permissions</h3>
          <ToggleRow id="toggle-input"     label="Allow Remote Input Control"   description="Enables keyboard and mouse simulation inputs" checked={s.allow_input_control}  onChange={s.toggleInputControl} />
          <ToggleRow id="toggle-audio"     label="Share Local Audio Output"     description="Captures and streams host system audio output" checked={s.audio_enabled}        onChange={s.toggleAudio} />
          <ToggleRow id="toggle-clipboard" label="Synchronize Clipboard Buffers" description="Allows copy/paste buffer syncing over network" checked={s.clipboard_enabled}   onChange={s.toggleClipboard} />
        </div>

        {/* System */}
        <div className="card">
          <h3 style={{ borderBottom: '1px solid var(--border)', paddingBottom: '6px', marginBottom: '10px' }}>Startup Configuration</h3>
          <ToggleRow id="toggle-startup"    label="Start with Windows Boot"     description="Launches Beacon background service automatically at user login" checked={s.start_with_windows} onChange={s.toggleStartWithWindows} />
          <ToggleRow id="toggle-unattended" label="Unattended Service Mode"     description="Maintains connection accessibility when lock screen is active" checked={s.unattended_mode} onChange={() => {}} />
        </div>

        {/* Sharing Indicator */}
        <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
          <h3 style={{ borderBottom: '1px solid var(--border)', paddingBottom: '6px' }}>Tray Notification Indicator</h3>
          <p style={{ fontSize: '0.8rem', color: 'var(--text-secondary)' }}>Configure system tray warning bubble visibility during active streams:</p>
          {(['always_show', 'hide_session', 'always_hide'] as IndicatorMode[]).map(mode => (
            <label
              key={mode}
              id={`indicator-${mode}`}
              style={{ display: 'flex', alignItems: 'center', gap: '10px', cursor: 'pointer', fontSize: '0.85rem', color: 'var(--text-primary)' }}
            >
              <input
                type="radio"
                name="indicator-mode"
                checked={s.indicator_mode === mode}
                onChange={() => s.setIndicatorMode(mode)}
                style={{ width: 'auto', cursor: 'pointer' }}
              />
              {{
                always_show:  'Show tray alert notifications (recommended safety mode)',
                hide_session: 'Suppress tray warnings for this session only',
                always_hide:  'Permanently deactivate active session tray popups',
              }[mode]}
            </label>
          ))}
        </div>

      </div>
    </div>
  );
};
