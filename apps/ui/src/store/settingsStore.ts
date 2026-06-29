import { create } from 'zustand';
import { invoke } from './ipc';

export type EncoderType = 'software' | 'nvenc' | 'amf' | 'qsv';
export type IndicatorMode = 'always_show' | 'hide_session' | 'always_hide';

interface Settings {
  bitrate_kbps: number;
  fps: number;
  encoder: EncoderType;
  audio_enabled: boolean;
  clipboard_enabled: boolean;
  allow_input_control: boolean;
  start_with_windows: boolean;
  unattended_mode: boolean;
  unattended_pin: string;
  indicator_mode: IndicatorMode;
  use_static_code: boolean;
  static_code: string;
  tls_enabled: boolean;
  adaptive_bitrate_enabled: boolean;
  signaling_server: string;

  // Actions
  setBitrate: (kbps: number) => void;
  setFps: (fps: number) => void;
  setEncoder: (enc: EncoderType) => void;
  toggleAudio: () => void;
  toggleClipboard: () => void;
  toggleInputControl: () => void;
  toggleStartWithWindows: () => void;
  toggleUnattendedMode: () => void;
  setUnattendedPin: (pin: string) => void;
  toggleUseStaticCode: () => void;
  setStaticCode: (code: string) => void;
  setIndicatorMode: (mode: IndicatorMode) => void;
  toggleTls: () => void;
  toggleAdaptiveBitrate: () => void;
  setSignalingServer: (url: string) => void;
  save: () => Promise<void>;
  load: () => Promise<void>;
}

export const useSettingsStore = create<Settings>((set, get) => ({
  bitrate_kbps: 8000,
  fps: 60,
  encoder: 'software',
  audio_enabled: false,
  clipboard_enabled: false,
  allow_input_control: true,
  start_with_windows: false,
  unattended_mode: false,
  unattended_pin: '',
  indicator_mode: 'always_show',
  use_static_code: false,
  static_code: '',
  tls_enabled: false,
  adaptive_bitrate_enabled: true,
  signaling_server: 'ws://127.0.0.1:45188',

  setBitrate: (kbps) => set({ bitrate_kbps: kbps }),
  setFps: (fps) => set({ fps }),
  setEncoder: (encoder) => set({ encoder }),
  toggleAudio: () => set(s => ({ audio_enabled: !s.audio_enabled })),
  toggleClipboard: () => set(s => ({ clipboard_enabled: !s.clipboard_enabled })),
  toggleInputControl: () => set(s => ({ allow_input_control: !s.allow_input_control })),
  toggleStartWithWindows: () => set(s => ({ start_with_windows: !s.start_with_windows })),
  toggleUnattendedMode: () => set(s => ({ unattended_mode: !s.unattended_mode })),
  setUnattendedPin: (pin) => set({ unattended_pin: pin }),
  toggleUseStaticCode: () => set(s => ({ use_static_code: !s.use_static_code })),
  setStaticCode: (code) => set({ static_code: code }),
  setIndicatorMode: (mode) => set({ indicator_mode: mode }),
  toggleTls: () => set(s => ({ tls_enabled: !s.tls_enabled })),
  toggleAdaptiveBitrate: () => set(s => ({ adaptive_bitrate_enabled: !s.adaptive_bitrate_enabled })),
  setSignalingServer: (signaling_server) => set({ signaling_server }),

  save: async () => {
    const s = get();
    await invoke('save_settings', {
      settings: {
        bitrate_kbps: s.bitrate_kbps,
        fps: s.fps,
        encoder: s.encoder,
        audio_enabled: s.audio_enabled,
        clipboard_enabled: s.clipboard_enabled,
        allow_input_control: s.allow_input_control,
        start_with_windows: s.start_with_windows,
        unattended_mode: s.unattended_mode,
        unattended_pin: s.unattended_pin,
        indicator_mode: s.indicator_mode,
        use_static_code: s.use_static_code,
        static_code: s.static_code,
        tls_enabled: s.tls_enabled,
        adaptive_bitrate_enabled: s.adaptive_bitrate_enabled,
        signaling_server: s.signaling_server,
      }
    });
  },

  load: async () => {
    try {
      const raw = await invoke<Record<string, unknown>>('load_settings');
      if (!raw || typeof raw !== 'object') {
        return;
      }
      // Only pick known data keys — never overwrite store action methods.
      const safeKeys = [
        'bitrate_kbps', 'fps', 'encoder', 'audio_enabled',
        'clipboard_enabled', 'allow_input_control', 'start_with_windows',
        'unattended_mode', 'unattended_pin', 'indicator_mode',
        'use_static_code', 'static_code', 'tls_enabled', 'adaptive_bitrate_enabled',
        'signaling_server',
      ] as const;
      const safe: Record<string, unknown> = {};
      for (const key of safeKeys) {
        if (key in raw && raw[key] !== undefined) {
          safe[key] = raw[key];
        }
      }
      set(safe as Partial<Settings>);
    } catch (e) {
      console.warn('No saved settings found, using defaults');
    }
  },
}));
