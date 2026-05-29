import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

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

  // Actions
  setBitrate: (kbps: number) => void;
  setFps: (fps: number) => void;
  setEncoder: (enc: EncoderType) => void;
  toggleAudio: () => void;
  toggleClipboard: () => void;
  toggleInputControl: () => void;
  toggleStartWithWindows: () => void;
  setIndicatorMode: (mode: IndicatorMode) => void;
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

  setBitrate: (kbps) => set({ bitrate_kbps: kbps }),
  setFps: (fps) => set({ fps }),
  setEncoder: (encoder) => set({ encoder }),
  toggleAudio: () => set(s => ({ audio_enabled: !s.audio_enabled })),
  toggleClipboard: () => set(s => ({ clipboard_enabled: !s.clipboard_enabled })),
  toggleInputControl: () => set(s => ({ allow_input_control: !s.allow_input_control })),
  toggleStartWithWindows: () => set(s => ({ start_with_windows: !s.start_with_windows })),
  setIndicatorMode: (mode) => set({ indicator_mode: mode }),

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
        indicator_mode: s.indicator_mode,
      }
    });
  },

  load: async () => {
    try {
      const raw = await invoke<Record<string, unknown>>('load_settings');
      // Only pick known data keys — never overwrite store action methods.
      const safeKeys = [
        'bitrate_kbps', 'fps', 'encoder', 'audio_enabled',
        'clipboard_enabled', 'allow_input_control', 'start_with_windows',
        'unattended_mode', 'unattended_pin', 'indicator_mode',
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
