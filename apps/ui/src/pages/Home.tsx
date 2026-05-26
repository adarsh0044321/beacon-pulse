import React from 'react';
import { Monitor, Link2, Settings as SettingsIcon } from 'lucide-react';
import type { Page } from '../App';

interface HomeProps {
  onNavigate: (page: Page) => void;
}

export const Home: React.FC<HomeProps> = ({ onNavigate }) => {
  return (
    <div className="page" style={{ justifyContent: 'center', alignItems: 'center' }}>
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: '32px', padding: '40px' }}>
        {/* Logo */}
        <div style={{ textAlign: 'center' }}>
          <div style={{ fontSize: '3rem', marginBottom: '8px' }}>🖥️</div>
          <h1>LANShare Window</h1>
          <p style={{ marginTop: '6px', fontSize: '0.875rem' }}>
            Share any window instantly over your local network
          </p>
        </div>

        {/* Mode buttons */}
        <div style={{ display: 'flex', gap: '16px', width: '100%', maxWidth: '360px' }}>
          <button
            id="btn-host"
            className="btn btn-primary btn-lg"
            style={{ flex: 1, flexDirection: 'column', gap: '10px', height: '130px', borderRadius: 'var(--radius-lg)' }}
            onClick={() => onNavigate('host')}
          >
            <Monitor size={32} />
            <span>Host</span>
            <span style={{ fontSize: '0.75rem', opacity: 0.7, fontWeight: 400 }}>Share a window</span>
          </button>

          <button
            id="btn-join"
            className="btn btn-ghost btn-lg"
            style={{ flex: 1, flexDirection: 'column', gap: '10px', height: '130px', borderRadius: 'var(--radius-lg)' }}
            onClick={() => onNavigate('client')}
          >
            <Link2 size={32} />
            <span>Join</span>
            <span style={{ fontSize: '0.75rem', opacity: 0.7, fontWeight: 400 }}>View a stream</span>
          </button>
        </div>

        {/* Settings link */}
        <button
          id="btn-settings"
          className="btn btn-ghost btn-sm"
          onClick={() => onNavigate('settings')}
        >
          <SettingsIcon size={14} />
          Settings
        </button>

        <span style={{ fontSize: '0.72rem', color: 'var(--text-muted)' }}>v0.1.0 · Windows 10+</span>
      </div>
    </div>
  );
};
