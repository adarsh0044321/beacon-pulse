import React from 'react';
import { Monitor, Link2, Settings as SettingsIcon } from 'lucide-react';
import type { Page } from '../App';

interface HomeProps {
  onNavigate: (page: Page) => void;
}

export const Home: React.FC<HomeProps> = ({ onNavigate }) => {
  return (
    <div className="page" style={{ justifyContent: 'center', alignItems: 'center' }}>
      <div 
        className="glass-panel" 
        style={{ 
          display: 'flex', 
          flexDirection: 'column', 
          alignItems: 'center', 
          gap: '36px', 
          padding: '50px 40px',
          width: '100%',
          maxWidth: '540px',
          boxShadow: '0 20px 50px rgba(0, 0, 0, 0.4)',
          border: '1px solid rgba(255, 255, 255, 0.1)',
          background: 'rgba(15, 13, 30, 0.45)',
        }}
      >
        {/* Logo Section */}
        <div style={{ textAlign: 'center' }}>
          <div 
            style={{ 
              fontSize: '4.5rem', 
              marginBottom: '16px',
              filter: 'drop-shadow(0 0 15px rgba(59, 130, 246, 0.45))',
              animation: 'logoPulse 3s ease-in-out infinite alternate'
            }}
          >
            🚀
          </div>
          <h1 style={{ fontSize: '2.4rem', fontWeight: 800 }}>Beacon & Pulse</h1>
          <p style={{ marginTop: '8px', fontSize: '0.95rem', color: 'var(--text-secondary)' }}>
            Ultra-low latency LAN remote streaming and control
          </p>
        </div>

        {/* Mode selection buttons */}
        <div style={{ display: 'flex', gap: '20px', width: '100%', maxWidth: '440px' }}>
          <button
            id="btn-host"
            className="btn btn-primary btn-lg"
            style={{ 
              flex: 1, 
              flexDirection: 'column', 
              gap: '12px', 
              height: '140px', 
              borderRadius: 'var(--radius-lg)',
              background: 'linear-gradient(135deg, rgba(59, 130, 246, 0.25) 0%, rgba(37, 99, 235, 0.05) 100%)',
              border: '1px solid rgba(59, 130, 246, 0.35)',
              boxShadow: '0 4px 20px rgba(59, 130, 246, 0.1)',
              color: '#fff',
            }}
            onClick={() => onNavigate('host')}
          >
            <Monitor size={36} style={{ color: 'var(--accent)', filter: 'drop-shadow(0 0 8px var(--accent-glow))' }} />
            <span style={{ fontWeight: 650, fontSize: '1.05rem', letterSpacing: '-0.01em' }}>Host Mode</span>
            <span style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', fontWeight: 400 }}>Share an application</span>
          </button>

          <button
            id="btn-join"
            className="btn btn-ghost btn-lg"
            style={{ 
              flex: 1, 
              flexDirection: 'column', 
              gap: '12px', 
              height: '140px', 
              borderRadius: 'var(--radius-lg)',
              background: 'linear-gradient(135deg, rgba(129, 140, 248, 0.2) 0%, rgba(99, 102, 241, 0.03) 100%)',
              border: '1px solid rgba(129, 140, 248, 0.25)',
              boxShadow: '0 4px 20px rgba(129, 140, 248, 0.05)',
              color: '#fff',
            }}
            onClick={() => onNavigate('client')}
          >
            <Link2 size={36} style={{ color: 'var(--accent-purple)', filter: 'drop-shadow(0 0 8px rgba(129, 140, 248, 0.3))' }} />
            <span style={{ fontWeight: 650, fontSize: '1.05rem', letterSpacing: '-0.01em' }}>Join Mode</span>
            <span style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', fontWeight: 400 }}>Watch & control screen</span>
          </button>
        </div>

        {/* Settings button */}
        <button
          id="btn-settings"
          className="btn btn-ghost btn-sm"
          style={{ 
            borderRadius: 'var(--radius-md)', 
            padding: '8px 16px',
            background: 'rgba(255, 255, 255, 0.02)',
            border: '1px solid var(--border)'
          }}
          onClick={() => onNavigate('settings')}
        >
          <SettingsIcon size={14} style={{ color: 'var(--text-secondary)' }} />
          <span>System Settings</span>
        </button>

        <span style={{ fontSize: '0.75rem', color: 'var(--text-muted)' }}>
          v0.1.0 · Secured LAN Connection · Windows 10+
        </span>
      </div>

      <style>{`
        @keyframes logoPulse {
          0% { transform: scale(1); filter: drop-shadow(0 0 10px rgba(59, 130, 246, 0.3)); }
          100% { transform: scale(1.06); filter: drop-shadow(0 0 25px rgba(59, 130, 246, 0.6)); }
        }
        #btn-host:hover {
          background: linear-gradient(135deg, rgba(59, 130, 246, 0.35) 0%, rgba(37, 99, 235, 0.1) 100%) !important;
          border-color: var(--accent) !important;
          transform: translateY(-4px) scale(1.02);
          box-shadow: 0 8px 30px rgba(59, 130, 246, 0.25) !important;
        }
        #btn-join:hover {
          background: linear-gradient(135deg, rgba(129, 140, 248, 0.3) 0%, rgba(99, 102, 241, 0.08) 100%) !important;
          border-color: var(--accent-purple) !important;
          transform: translateY(-4px) scale(1.02);
          box-shadow: 0 8px 30px rgba(129, 140, 248, 0.2) !important;
        }
      `}</style>
    </div>
  );
};
