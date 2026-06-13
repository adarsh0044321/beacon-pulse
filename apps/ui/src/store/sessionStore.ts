import { create } from 'zustand';
import { invoke } from './ipc';

export interface WindowInfo {
  hwnd: number;
  title: string;
  process_name: string;
  process_id: number;
  width: number;
  height: number;
  is_minimized: boolean;
  app_kind: 'Win32' | 'UWP' | 'Chromium' | 'DirectX' | 'OpenGL' | 'Vulkan' | 'RDP' | 'Unknown';
  suspends_render_when_minimized: boolean;
}

export interface ConnectedClient {
  client_id: string;
  display_name: string;
  addr: string;
  permissions: {
    input_control: boolean;
    clipboard: boolean;
    audio: boolean;
  };
  stats: {
    fps: number;
    latency_ms: number;
    bitrate_kbps: number;
  };
}

export interface DiscoveredHost {
  name: string;
  address: string;
  port: number;
  /** Protocol version from mDNS TXT record. Undefined for older hosts. */
  version?: string;
  mac?: string;
  tls?: boolean;
  lastSeen?: number;
}

export interface Stats {
  fps: number;
  encode_ms: number;
  latency_ms: number;
  bitrate_kbps: number;
  client_count: number;
  /** Phase 5: true when the zero-copy GPU texture path is live for the current session. */
  gpu_path_active: boolean;
}

/// Phase 3: encoder status pushed by service on session start
export interface EncoderInfo {
  encoder_name: string;  // e.g. "NVENC", "AMF", "QuickSync", "OpenH264"
  vendor: string;        // e.g. "NVIDIA", "AMD", "Intel", "Software"
  hw_accelerated: boolean;
}

export interface MonitorInfo {
  handle: number;
  name: string;
  width: number;
  height: number;
  is_primary: boolean;
  refresh_rate: number;
  index: number;
}

export interface ProcessInfo {
  pid: number;
  name: string;
  threads: number;
}

interface SessionState {
  // Sharing state
  isSharing: boolean;
  activeHwnd: number | null;
  activeTarget: any | null;
  pairingCode: string | null;
  pairingExpiresIn: number;
  connectedClients: ConnectedClient[];
  stats: Stats;

  // Host window picker
  availableWindows: WindowInfo[];
  windowsLoading: boolean;

  // Host monitor picker
  availableMonitors: MonitorInfo[];
  monitorsLoading: boolean;

  // Client connection
  discoveredHosts: DiscoveredHost[];
  hostsLoading: boolean;
  connectedHost: DiscoveredHost | null;

  // Remote Task Manager
  hostProcesses: ProcessInfo[];
  processesLoading: boolean;

  // Actions
  fetchWindows: () => Promise<void>;
  fetchHostProcesses: () => Promise<void>;
  killHostProcess: (pid: number) => Promise<void>;
  setHostProcesses: (processes: ProcessInfo[]) => void;
  fetchMonitors: () => Promise<void>;
  startShare: (target: any) => Promise<void>;
  stopShare: () => Promise<void>;
  generatePairingCode: () => Promise<void>;
  kickClient: (clientId: string) => Promise<void>;
  fetchActiveClients: () => Promise<void>;
  discoverHosts: () => Promise<void>;
  connectToHost: (host: DiscoveredHost, code: string) => Promise<void>;
  disconnectFromHost: () => void;
  updateStats: (stats: Stats) => void;
  setShareActive: (active: boolean, target?: any) => void;
  addConnectedClient: (client: ConnectedClient) => void;
  removeConnectedClient: (clientId: string) => void;
  /// Phase 3
  encoderInfo: EncoderInfo | null;
  setEncoderInfo: (info: EncoderInfo) => void;
  setBitrate: (kbps: number) => void;
}

export const useSessionStore = create<SessionState>((set, get) => ({
  isSharing: false,
  activeHwnd: null,
  activeTarget: null,
  pairingCode: null,
  pairingExpiresIn: 120,
  connectedClients: [],
  stats: { fps: 0, encode_ms: 0, latency_ms: 0, bitrate_kbps: 0, client_count: 0, gpu_path_active: false },

  availableWindows: [],
  windowsLoading: false,

  availableMonitors: [],
  monitorsLoading: false,

  discoveredHosts: [],
  hostsLoading: false,
  connectedHost: null,

  hostProcesses: [],
  processesLoading: false,

  encoderInfo: null,

  fetchWindows: async () => {
    const isFirstLoad = get().availableWindows.length === 0;
    if (isFirstLoad) {
      set({ windowsLoading: true });
    }
    try {
      const windows = await invoke<WindowInfo[]>('list_windows');
      set({ availableWindows: windows, windowsLoading: false });
    } catch (e) {
      console.error('Failed to list windows:', e);
      set({ windowsLoading: false });
    }
  },

  fetchMonitors: async () => {
    const isFirstLoad = get().availableMonitors.length === 0;
    if (isFirstLoad) {
      set({ monitorsLoading: true });
    }
    try {
      const monitors = await invoke<MonitorInfo[]>('list_monitors');
      set({ availableMonitors: monitors, monitorsLoading: false });
    } catch (e) {
      console.error('Failed to list monitors:', e);
      set({ monitorsLoading: false });
    }
  },

  startShare: async (target: any) => {
    try {
      await invoke('start_share', { target });
      let activeHwnd = null;
      if (target && target.kind === 'window') {
        activeHwnd = target.data;
      } else if (target && target.kind === 'display') {
        activeHwnd = target.data;
      }
      set({ isSharing: true, activeTarget: target, activeHwnd });
    } catch (e) {
      console.error('Failed to start share:', e);
    }
  },

  stopShare: async () => {
    try {
      await invoke('stop_share');
      set({
        isSharing: false,
        activeHwnd: null,
        activeTarget: null,
        connectedClients: [],
        pairingCode: null,
        encoderInfo: null,
      });
    } catch (e) {
      console.error('Failed to stop share:', e);
    }
  },

  generatePairingCode: async () => {
    try {
      const result = await invoke<{ code: string; expires_in: number }>('generate_pairing_code');
      set({ pairingCode: result.code, pairingExpiresIn: result.expires_in });
    } catch (e) {
      console.error('Failed to generate code:', e);
    }
  },

  kickClient: async (clientId: string) => {
    try {
      await invoke('kick_client', { clientId });
      set(state => ({
        connectedClients: state.connectedClients.filter(c => c.client_id !== clientId)
      }));
    } catch (e) {
      console.error('Failed to kick client:', e);
    }
  },

  fetchActiveClients: async () => {
    try {
      const clients = await invoke<{ client_id: string; addr: string; display_name: string }[]>('get_active_clients');
      set({
        connectedClients: clients.map(c => ({
          client_id: c.client_id,
          display_name: c.display_name,
          addr: c.addr,
          permissions: { input_control: true, clipboard: true, audio: true },
          stats: { fps: 60, latency_ms: 0, bitrate_kbps: 0 }
        }))
      });
    } catch (e) {
      console.error('Failed to fetch active clients:', e);
    }
  },

  discoverHosts: async () => {
    set({ hostsLoading: true });
    try {
      const hosts = await invoke<DiscoveredHost[]>('discover_hosts');
      const now = Date.now();
      const existing = get().discoveredHosts;
      
      // Map of key (name) to host
      const hostMap = new Map<string, DiscoveredHost>();
      
      // 1. Populate map with existing hosts
      existing.forEach(h => {
        hostMap.set(h.name, h);
      });
      
      // 2. Add or update with newly discovered hosts
      hosts.forEach(h => {
        hostMap.set(h.name, {
          ...h,
          lastSeen: now
        });
      });
      
      // 3. Filter out hosts that haven't been seen for more than 30 seconds
      const mergedHosts = Array.from(hostMap.values()).filter(h => {
        const lastSeen = h.lastSeen ?? now;
        return now - lastSeen < 30000; // 30 seconds lease
      });
      
      set({ discoveredHosts: mergedHosts, hostsLoading: false });
    } catch (e) {
      console.error('Failed to discover hosts:', e);
      set({ hostsLoading: false });
    }
  },

  connectToHost: async (host: DiscoveredHost, code: string) => {
    try {
      await invoke('connect_to_host', { address: host.address, port: host.port, code, tls: host.tls ?? false });
      set({ connectedHost: host });
    } catch (e) {
      console.error('Failed to connect:', e);
      throw e;
    }
  },

  disconnectFromHost: () => {
    invoke('disconnect_from_host').catch(console.error);
    set({ connectedHost: null });
  },

  updateStats: (stats: Stats) => set({ stats }),

  // Pure state mutation — called by IPC push events (share_started / share_stopped)
  // so we don't invoke() the Tauri command again (which would create a feedback loop).
  setShareActive: (active: boolean, target?: any) =>
    set(active
      ? {
          isSharing: true,
          activeTarget: target ?? null,
          activeHwnd: target && target.kind === 'window' ? target.data : (typeof target === 'number' ? target : null)
        }
      : { isSharing: false, activeTarget: null, activeHwnd: null, connectedClients: [] }),

  addConnectedClient: (client: ConnectedClient) => set((state) => {
    if (state.connectedClients.some(c => c.client_id === client.client_id)) {
      return state;
    }
    return { connectedClients: [...state.connectedClients, client] };
  }),

  removeConnectedClient: (clientId: string) => set((state) => ({
    connectedClients: state.connectedClients.filter(c => c.client_id !== clientId)
  })),

  // Phase 3
  setEncoderInfo: (info: EncoderInfo) => set({ encoderInfo: info }),
  setBitrate: (kbps: number) => {
    invoke('set_bitrate', { kbps }).catch(console.error);
  },

  fetchHostProcesses: async () => {
    set({ processesLoading: true });
    try {
      await invoke('list_host_processes');
    } catch (e) {
      console.error('Failed to list host processes:', e);
      set({ processesLoading: false });
    }
  },

  killHostProcess: async (pid: number) => {
    try {
      await invoke('kill_host_process', { pid });
    } catch (e) {
      console.error('Failed to kill host process:', e);
    }
  },

  setHostProcesses: (processes: ProcessInfo[]) => {
    set({ hostProcesses: processes, processesLoading: false });
  },
}));
