import React, { useEffect, useRef, useState, useCallback } from 'react';
import { ArrowLeft, RefreshCw, Wifi, WifiOff, Maximize2, Minimize2, Activity, Settings as SettingsIcon, Play, Terminal, Users, Trash2, History, Cpu } from 'lucide-react';
import type { Page } from '../App';
import { useSessionStore, type DiscoveredHost } from '../store/sessionStore';
import { useToastStore } from '../store/toastStore';
import { invoke, listen } from '../store/ipc';
import { DebugOverlay } from '../components/DebugOverlay';
import { FileManager } from '../components/FileManager';

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

// Helper to convert H.264 Annex-B bitstream (delimited by 0x00000001 or 0x000001 start codes)
// to AVCC format (length-prefixed NAL units with 4-byte length prefix).
function annexBToAvcc(bytes: Uint8Array): Uint8Array {
  const len = bytes.length;
  const offsets: number[] = [];
  
  let i = 0;
  while (i < len - 2) {
    if (bytes[i] === 0 && bytes[i + 1] === 0) {
      if (bytes[i + 2] === 1) {
        offsets.push(i);
        i += 3;
        continue;
      } else if (i + 3 < len && bytes[i + 2] === 0 && bytes[i + 3] === 1) {
        offsets.push(i);
        i += 4;
        continue;
      }
    }
    i++;
  }
  
  if (offsets.length === 0) {
    return bytes;
  }
  
  const nalUnits: Uint8Array[] = [];
  for (let idx = 0; idx < offsets.length; idx++) {
    const start = offsets[idx];
    const nextStart = idx + 1 < offsets.length ? offsets[idx + 1] : len;
    
    let startCodeLen = 3;
    if (start + 3 < len && bytes[start] === 0 && bytes[start + 1] === 0 && bytes[start + 2] === 0 && bytes[start + 3] === 1) {
      startCodeLen = 4;
    }
    
    const payloadStart = start + startCodeLen;
    let payloadEnd = nextStart;
    
    while (payloadEnd > payloadStart && bytes[payloadEnd - 1] === 0) {
      payloadEnd--;
    }
    
    if (payloadEnd > payloadStart) {
      nalUnits.push(bytes.subarray(payloadStart, payloadEnd));
    }
  }
  
  if (nalUnits.length === 0) {
    return bytes;
  }
  
  let totalSize = 0;
  for (const nal of nalUnits) {
    totalSize += 4 + nal.length;
  }
  
  const avcc = new Uint8Array(totalSize);
  let writeOffset = 0;
  for (const nal of nalUnits) {
    const nalLen = nal.length;
    avcc[writeOffset] = (nalLen >> 24) & 0xff;
    avcc[writeOffset + 1] = (nalLen >> 16) & 0xff;
    avcc[writeOffset + 2] = (nalLen >> 8) & 0xff;
    avcc[writeOffset + 3] = nalLen & 0xff;
    avcc.set(nal, writeOffset + 4);
    writeOffset += 4 + nalLen;
  }
  
  return avcc;
}

function arraysEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

// Helper to construct AVCDecoderConfigurationRecord (description) from H.264 SPS/PPS NAL units
function buildAvcDescription(bytes: Uint8Array): Uint8Array | null {
  const len = bytes.length;
  let sps: Uint8Array | null = null;
  let pps: Uint8Array | null = null;
  
  // Find all start codes and extract NAL units
  let i = 0;
  const offsets: number[] = [];
  while (i < len - 2) {
    if (bytes[i] === 0 && bytes[i + 1] === 0) {
      if (bytes[i + 2] === 1) {
        offsets.push(i);
        i += 3;
        continue;
      } else if (i + 3 < len && bytes[i + 2] === 0 && bytes[i + 3] === 1) {
        offsets.push(i);
        i += 4;
        continue;
      }
    }
    i++;
  }
  
  for (let idx = 0; idx < offsets.length; idx++) {
    const start = offsets[idx];
    const nextStart = idx + 1 < offsets.length ? offsets[idx + 1] : len;
    
    let startCodeLen = 3;
    if (start + 3 < len && bytes[start] === 0 && bytes[start + 1] === 0 && bytes[start + 2] === 0 && bytes[start + 3] === 1) {
      startCodeLen = 4;
    }
    
    const payloadStart = start + startCodeLen;
    let payloadEnd = nextStart;
    while (payloadEnd > payloadStart && bytes[payloadEnd - 1] === 0) {
      payloadEnd--;
    }
    
    if (payloadEnd > payloadStart) {
      const nal = bytes.subarray(payloadStart, payloadEnd);
      const nalType = nal[0] & 0x1f;
      if (nalType === 7) sps = nal;
      if (nalType === 8) pps = nal;
    }
  }
  
  if (!sps || !pps) {
    return null;
  }
  
  const profileIdc = sps[1];
  const constraints = sps[2];
  const levelIdc = sps[3];
  
  const descSize = 11 + sps.length + 3 + pps.length;
  const desc = new Uint8Array(descSize);
  
  desc[0] = 1; // configurationVersion
  desc[1] = profileIdc; // AVCProfileIndication
  desc[2] = constraints; // profile_compatibility
  desc[3] = levelIdc; // AVCLevelIndication
  desc[4] = 0xff; // lengthSizeMinusOne | 0xfc (reserved) -> 0xff (4-byte length)
  desc[5] = 0xe1; // numOfSequenceParameterSets | 0xe0 (reserved) -> 0xe1 (1 SPS)
  
  desc[6] = (sps.length >> 8) & 0xff;
  desc[7] = sps.length & 0xff;
  desc.set(sps, 8);
  
  const ppsOffset = 8 + sps.length;
  desc[ppsOffset] = 1; // numOfPictureParameterSets
  desc[ppsOffset + 1] = (pps.length >> 8) & 0xff;
  desc[ppsOffset + 2] = pps.length & 0xff;
  desc.set(pps, ppsOffset + 3);
  
  return desc;
}

// Helper to parse H.264 SPS (Sequence Parameter Set) from Annex-B bitstream
// to extract profile_idc, constraint_set_flags, and level_idc and construct the WebCodecs codec string.
function parseSpsCodec(bytes: Uint8Array): string | null {
  const len = bytes.length;
  let i = 0;
  while (i < len - 4) {
    let startCodeLen = 0;
    if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
      startCodeLen = 3;
    } else if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 0 && bytes[i + 3] === 1) {
      startCodeLen = 4;
    }

    if (startCodeLen > 0) {
      const nalOffset = i + startCodeLen;
      if (nalOffset < len) {
        const nalHeader = bytes[nalOffset];
        const nalType = nalHeader & 0x1F;
        if (nalType === 7) { // SPS NAL unit
          if (nalOffset + 3 < len) {
            const profileIdc = bytes[nalOffset + 1];
            const profileConstraints = bytes[nalOffset + 2];
            const levelIdc = bytes[nalOffset + 3];
            
            const validProfiles = [66, 77, 88, 100, 110, 122, 244];
            if (validProfiles.includes(profileIdc)) {
              const toHex = (val: number) => val.toString(16).padStart(2, '0').toLowerCase();
              // For H.264 WebCodecs, using a high level like Level 5.1 (0x33) is recommended for compatibility
              // so the decoder is prepared to handle any resolution/bitrate up to that level.
              // We also map Baseline Profile (66) constraints to c0 (Constrained Baseline) which is widely supported.
              const constraints = profileIdc === 66 ? 0xc0 : profileConstraints;
              return `avc1.${toHex(profileIdc)}${toHex(constraints)}33`;
            }
          }
        }
      }
      i += startCodeLen;
    } else {
      i++;
    }
  }
  return null;
}

function useMultiWebCodecsDecoder() {
  const decodersRef = useRef<Map<number, {
    decoder: VideoDecoder | null;
    canvas: HTMLCanvasElement | null;
    frameCount: number;
    lastFpsCheck: number;
    stats: DecoderStats;
    configuredCodec: string | null;
    configuredDescription: Uint8Array | null;
    chunksFed: number;
    hardwarePreference: 'no-preference' | 'prefer-hardware' | 'prefer-software';
  }>>(new Map());
  const [displayStats, setDisplayStats] = useState<Record<number, DecoderStats>>({});
  const [errors, setErrors] = useState<Record<number, string>>({});

  const registerCanvas = useCallback((displayId: number, canvas: HTMLCanvasElement | null) => {
    let instance = decodersRef.current.get(displayId);
    if (!instance) {
      instance = {
        decoder: null,
        canvas: null,
        frameCount: 0,
        lastFpsCheck: Date.now(),
        stats: { fps: 0, decodeMs: 0, frames: 0, keyframes: 0 },
        configuredCodec: null,
        configuredDescription: null,
        chunksFed: 0,
        hardwarePreference: 'no-preference',
      };
      decodersRef.current.set(displayId, instance);
    }
    
    if (canvas && instance.canvas !== canvas) {
      instance.canvas = canvas;
      if (instance.decoder) {
        instance.decoder.close();
        instance.decoder = null;
        instance.configuredCodec = null;
        instance.configuredDescription = null;
      }
    }
  }, []);

  const initDecoder = useCallback((displayId: number, width: number, height: number, codecStr: string = 'avc1.640033', description?: Uint8Array) => {
    let instance = decodersRef.current.get(displayId);
    if (!instance || !instance.canvas) return;

    if (instance.decoder && instance.decoder.state !== 'closed') {
      instance.decoder.close();
    }

    const canvas = instance.canvas;
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d')!;

    try {
      if (typeof VideoDecoder === 'undefined') {
        throw new Error(
          "WebCodecs (VideoDecoder) is not supported in this browser/context. " +
          "WebCodecs requires a Secure Context (HTTPS or localhost) in Chrome/Firefox. " +
          "If you are accessing this UI over a LAN IP (e.g., http://192.168.x.x:45200), " +
          "please use localhost or enable the 'Insecure origins treated as secure' flag in chrome://flags."
        );
      }

      const decoder = new VideoDecoder({
        output: (frame: VideoFrame) => {
          if (instance.canvas) {
            ctx.drawImage(frame, 0, 0, instance.canvas.width, instance.canvas.height);
          }
          frame.close();
          instance.frameCount++;
          instance.chunksFed = 0; // successfully decoded a frame

          const now = Date.now();
          if (now - instance.lastFpsCheck >= 1000) {
            const elapsed = (now - instance.lastFpsCheck) / 1000;
            const fps = Math.round(instance.frameCount / elapsed);
            instance.stats = {
              ...instance.stats,
              fps,
              frames: instance.stats.frames + instance.frameCount,
            };
            setDisplayStats(prev => ({
              ...prev,
              [displayId]: { ...instance.stats }
            }));
            instance.frameCount = 0;
            instance.lastFpsCheck = now;
          }
        },
        error: (e: DOMException) => {
          console.error(`[WebCodecs] Decoder error on display ${displayId}:`, e);
          setErrors(prev => ({ ...prev, [displayId]: `Decoder error: ${e.message}` }));
        },
      });

      console.log(`[WebCodecs] Configuring decoder on display ${displayId} with codec: ${codecStr} (Accel: ${instance.hardwarePreference}, HasDesc: ${!!description})`);
      const config: VideoDecoderConfig = {
        codec: codecStr,
        codedWidth: width,
        codedHeight: height,
        optimizeForLatency: true,
        hardwareAcceleration: instance.hardwarePreference,
      };
      if (description) {
        config.description = description;
      }
      decoder.configure(config);

      instance.decoder = decoder;
      instance.configuredCodec = codecStr;
      instance.configuredDescription = description || null;
      setErrors(prev => {
        const copy = { ...prev };
        delete copy[displayId];
        return copy;
      });
    } catch (e: any) {
      console.error(`[WebCodecs] Failed to initialize decoder on display ${displayId}:`, e);
      setErrors(prev => ({ ...prev, [displayId]: e.message || String(e) }));
    }
  }, []);

  const feedChunk = useCallback((
    displayId: number,
    data: string,
    timestampUs: number,
    isKeyframe: boolean,
    width: number,
    height: number,
  ) => {
    let instance = decodersRef.current.get(displayId);
    if (!instance || !instance.canvas) return;

    const raw = atob(data);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);

    let targetCodec = instance.configuredCodec || 'avc1.640033';
    let description: Uint8Array | undefined = undefined;
    if (isKeyframe) {
      const parsed = parseSpsCodec(bytes);
      if (parsed) {
        targetCodec = parsed;
      }
      const desc = buildAvcDescription(bytes);
      if (desc) {
        description = desc;
      }
    }

    instance.chunksFed++;
    if (instance.chunksFed > 45 && instance.frameCount === 0 && instance.hardwarePreference !== 'prefer-software') {
      console.warn(`[WebCodecs] Fed ${instance.chunksFed} chunks but decoded 0 frames. Triggering automatic fallback to software decoding.`);
      instance.hardwarePreference = 'prefer-software';
      initDecoder(displayId, width, height, targetCodec, description);
      instance.chunksFed = 0;
    }

    const needsInit = !instance.decoder || 
      instance.decoder.state === 'closed' || 
      instance.configuredCodec !== targetCodec ||
      (description && (!instance.configuredDescription || !arraysEqual(instance.configuredDescription, description)));

    if (needsInit) {
      initDecoder(displayId, width, height, targetCodec, description);
    }
    const decoder = instance.decoder;
    if (!decoder || decoder.state !== 'configured') return;

    // Convert Annex-B start-coded NALs to AVCC length-prefixed format for WebCodecs
    const avccBytes = annexBToAvcc(bytes);

    const chunk = new EncodedVideoChunk({
      type: isKeyframe ? 'key' : 'delta',
      timestamp: timestampUs,
      data: avccBytes,
    });

    try {
      const startDecode = performance.now();
      decoder.decode(chunk);
      const endDecode = performance.now();

      if (isKeyframe) {
        instance.stats = {
          ...instance.stats,
          keyframes: instance.stats.keyframes + 1,
          decodeMs: Math.round(endDecode - startDecode),
        };
        setDisplayStats(prev => ({
          ...prev,
          [displayId]: { ...instance.stats }
        }));
      }
    } catch (e) {
      console.warn(`[WebCodecs] decode() failed for display ${displayId}:`, e);
    }
  }, [initDecoder]);

  const closeDecoder = useCallback((displayId: number) => {
    const instance = decodersRef.current.get(displayId);
    if (instance) {
      if (instance.decoder && instance.decoder.state !== 'closed') {
        instance.decoder.close();
      }
      instance.decoder = null;
    }
  }, []);

  const closeAllDecoders = useCallback(() => {
    decodersRef.current.forEach((instance) => {
      if (instance.decoder && instance.decoder.state !== 'closed') {
        instance.decoder.close();
      }
      instance.decoder = null;
    });
    setDisplayStats({});
    setErrors({});
  }, []);

  return { feedChunk, registerCanvas, closeDecoder, closeAllDecoders, displayStats, errors };
}

// ─────────────────────────────────────────────────────────────────────────────
// Key mapping from KeyboardEvent.code to Windows Scan Code and Extended Key Flag
// ─────────────────────────────────────────────────────────────────────────────
const KEY_MAP: Record<string, { scan: number; extended?: boolean }> = {
  Escape: { scan: 0x01 },
  Digit1: { scan: 0x02 },
  Digit2: { scan: 0x03 },
  Digit3: { scan: 0x04 },
  Digit4: { scan: 0x05 },
  Digit5: { scan: 0x06 },
  Digit6: { scan: 0x07 },
  Digit7: { scan: 0x08 },
  Digit8: { scan: 0x09 },
  Digit9: { scan: 0x0A },
  Digit0: { scan: 0x0B },
  Minus: { scan: 0x0C },
  Equal: { scan: 0x0D },
  Backspace: { scan: 0x0E },
  Tab: { scan: 0x0F },
  KeyQ: { scan: 0x10 },
  KeyW: { scan: 0x11 },
  KeyE: { scan: 0x12 },
  KeyR: { scan: 0x13 },
  KeyT: { scan: 0x14 },
  KeyY: { scan: 0x15 },
  KeyU: { scan: 0x16 },
  KeyI: { scan: 0x17 },
  KeyO: { scan: 0x18 },
  KeyP: { scan: 0x19 },
  BracketLeft: { scan: 0x1A },
  BracketRight: { scan: 0x1B },
  Enter: { scan: 0x1C },
  ControlLeft: { scan: 0x1D },
  KeyA: { scan: 0x1E },
  KeyS: { scan: 0x1F },
  KeyD: { scan: 0x20 },
  KeyF: { scan: 0x21 },
  KeyG: { scan: 0x22 },
  KeyH: { scan: 0x23 },
  KeyJ: { scan: 0x24 },
  KeyK: { scan: 0x25 },
  KeyL: { scan: 0x26 },
  Semicolon: { scan: 0x27 },
  Quote: { scan: 0x28 },
  Backquote: { scan: 0x29 },
  ShiftLeft: { scan: 0x2A },
  Backslash: { scan: 0x2B },
  KeyZ: { scan: 0x2C },
  KeyX: { scan: 0x2D },
  KeyC: { scan: 0x2E },
  KeyV: { scan: 0x2F },
  KeyB: { scan: 0x30 },
  KeyN: { scan: 0x31 },
  KeyM: { scan: 0x32 },
  Comma: { scan: 0x33 },
  Period: { scan: 0x34 },
  Slash: { scan: 0x35 },
  ShiftRight: { scan: 0x36 },
  NumpadMultiply: { scan: 0x37 },
  AltLeft: { scan: 0x38 },
  Space: { scan: 0x39 },
  CapsLock: { scan: 0x3A },
  F1: { scan: 0x3B },
  F2: { scan: 0x3C },
  F3: { scan: 0x3D },
  F4: { scan: 0x3E },
  F5: { scan: 0x3F },
  F6: { scan: 0x40 },
  F7: { scan: 0x41 },
  F8: { scan: 0x42 },
  F9: { scan: 0x43 },
  F10: { scan: 0x44 },
  NumLock: { scan: 0x45 },
  ScrollLock: { scan: 0x46 },
  Numpad7: { scan: 0x47 },
  Numpad8: { scan: 0x48 },
  Numpad9: { scan: 0x49 },
  NumpadSubtract: { scan: 0x4A },
  Numpad4: { scan: 0x4B },
  Numpad5: { scan: 0x4C },
  Numpad6: { scan: 0x4D },
  NumpadAdd: { scan: 0x4E },
  Numpad1: { scan: 0x4F },
  Numpad2: { scan: 0x50 },
  Numpad3: { scan: 0x51 },
  Numpad0: { scan: 0x52 },
  NumpadDecimal: { scan: 0x53 },
  F11: { scan: 0x57 },
  F12: { scan: 0x58 },

  // Extended Keys
  NumpadEnter: { scan: 0x1C, extended: true },
  ControlRight: { scan: 0x1D, extended: true },
  NumpadDivide: { scan: 0x35, extended: true },
  AltRight: { scan: 0x38, extended: true },
  Home: { scan: 0x47, extended: true },
  ArrowUp: { scan: 0x48, extended: true },
  PageUp: { scan: 0x49, extended: true },
  ArrowLeft: { scan: 0x4B, extended: true },
  ArrowRight: { scan: 0x4D, extended: true },
  End: { scan: 0x4F, extended: true },
  ArrowDown: { scan: 0x50, extended: true },
  PageDown: { scan: 0x51, extended: true },
  Insert: { scan: 0x52, extended: true },
  Delete: { scan: 0x53, extended: true },
  MetaLeft: { scan: 0x5B, extended: true },
  MetaRight: { scan: 0x5C, extended: true },
  ContextMenu: { scan: 0x5D, extended: true },
};

// ─────────────────────────────────────────────────────────────────────────────
// Main Component
// ─────────────────────────────────────────────────────────────────────────────

export const Client: React.FC<ClientProps> = ({ onNavigate }) => {
  const {
    discoveredHosts, hostsLoading, discoverHosts,
    connectedHost, connectToHost, disconnectFromHost,
    stats: sessionStats,
    hostProcesses, processesLoading, fetchHostProcesses,
    killHostProcess, setHostProcesses,
  } = useSessionStore();

  const { addToast } = useToastStore();

  const [activeDisplays, setActiveDisplays] = useState<number[]>([0]);
  const [activeDisplayTab, setActiveDisplayTab] = useState<number | 'grid'>(0);

  const { feedChunk, registerCanvas, closeDecoder, closeAllDecoders, displayStats, errors: decodeErrors } =
    useMultiWebCodecsDecoder();

  const activeStatsId = typeof activeDisplayTab === 'number' ? activeDisplayTab : 0;
  const decodeStats = displayStats[activeStatsId] || { fps: 0, decodeMs: 0, frames: 0, keyframes: 0 };
  const decodeError = decodeErrors[activeStatsId] || null;

  const [tab, setTab] = useState<'join' | 'stream' | 'performance' | 'logs' | 'remote_manager'>('join');
  const [pairingInput, setPairingInput] = useState('');
  const [connectingHost, setConnectingHost] = useState<DiscoveredHost | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [manualIp, setManualIp] = useState('');
  const [streamConnected, setStreamConnected] = useState(false);
  const [cursorShape, setCursorShape] = useState('default');
  const [fileTransferring, setFileTransferring] = useState<string | null>(null);
  const [fileProgress, setFileProgress] = useState(0);
  const [wolMac, setWolMac] = useState(() => localStorage.getItem('lanshare_last_mac') || '');
  const [wolSending, setWolSending] = useState(false);
  const [wolStatus, setWolStatus] = useState('');
  const pressedKeysRef = useRef<Map<string, { keyCode: number; scan: number; extended: boolean }>>(new Map());
  const [packetLossPct, setPacketLossPct] = useState(0);
  const [playerMemoryBytes, setPlayerMemoryBytes] = useState<number>(0);
  const [recentConnections, setRecentConnections] = useState<DiscoveredHost[]>([]);
  const [scalingMode, setScalingMode] = useState<'fit' | 'stretch' | 'original'>('fit');
  const [processSearch, setProcessSearch] = useState('');
  const [remoteSubTab, setRemoteSubTab] = useState<'processes' | 'files'>('processes');
  const [killConfirmPid, setKillConfirmPid] = useState<number | null>(null);
  const [killConfirmName, setKillConfirmName] = useState('');

  // Session Recording State
  const [isRecording, setIsRecording] = useState(false);
  const [recordingTime, setRecordingTime] = useState(0);
  const recordingChunksRef = useRef<Blob[]>([]);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const recordingTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const formatTime = (secs: number) => {
    const m = Math.floor(secs / 60).toString().padStart(2, '0');
    const s = (secs % 60).toString().padStart(2, '0');
    return `${m}:${s}`;
  };

  const startRecording = () => {
    const activeStatsId = typeof activeDisplayTab === 'number' ? activeDisplayTab : 0;
    const canvas = document.getElementById(`stream-canvas-${activeStatsId}`) as HTMLCanvasElement;
    if (!canvas) {
      addToast('Recording Error', 'No active stream viewport found.', 'error');
      return;
    }

    try {
      recordingChunksRef.current = [];
      const stream = canvas.captureStream(30);
      
      const recorder = new MediaRecorder(stream, {
        mimeType: 'video/webm;codecs=vp9'
      });

      recorder.ondataavailable = (e) => {
        if (e.data && e.data.size > 0) {
          recordingChunksRef.current.push(e.data);
        }
      };

      recorder.onstop = () => {
        const blob = new Blob(recordingChunksRef.current, { type: 'video/webm' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
        a.download = `Session-Recording-${timestamp}.webm`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);
        addToast('Recording Saved', 'Session recording has been downloaded.', 'success');
      };

      recorder.start(1000);
      mediaRecorderRef.current = recorder;
      setIsRecording(true);
      setRecordingTime(0);

      recordingTimerRef.current = setInterval(() => {
        setRecordingTime(t => t + 1);
      }, 1000);

      addToast('Recording Started', 'Capturing screen stream.', 'success');
    } catch (err) {
      console.error('Failed to start MediaRecorder:', err);
      try {
        const stream = canvas.captureStream(30);
        const recorder = new MediaRecorder(stream);
        recorder.ondataavailable = (e) => {
          if (e.data && e.data.size > 0) {
            recordingChunksRef.current.push(e.data);
          }
        };
        recorder.onstop = () => {
          const blob = new Blob(recordingChunksRef.current, { type: 'video/webm' });
          const url = URL.createObjectURL(blob);
          const a = document.createElement('a');
          a.href = url;
          const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
          a.download = `Session-Recording-${timestamp}.webm`;
          document.body.appendChild(a);
          a.click();
          document.body.removeChild(a);
          URL.revokeObjectURL(url);
          addToast('Recording Saved', 'Session recording has been downloaded.', 'success');
        };
        recorder.start(1000);
        mediaRecorderRef.current = recorder;
        setIsRecording(true);
        setRecordingTime(0);

        recordingTimerRef.current = setInterval(() => {
          setRecordingTime(t => t + 1);
        }, 1000);

        addToast('Recording Started', 'Capturing screen stream (compatibility mode).', 'success');
      } catch (fallbackErr: any) {
        addToast('Recording Failed', `Could not initialize media recorder: ${fallbackErr.message}`, 'error');
      }
    }
  };

  const stopRecording = () => {
    if (mediaRecorderRef.current && mediaRecorderRef.current.state !== 'inactive') {
      mediaRecorderRef.current.stop();
    }
    if (recordingTimerRef.current) {
      clearInterval(recordingTimerRef.current);
      recordingTimerRef.current = null;
    }
    setIsRecording(false);
  };

  useEffect(() => {
    return () => {
      if (recordingTimerRef.current) {
        clearInterval(recordingTimerRef.current);
      }
    };
  }, []);

  // Load recent connections on load/tab switch
  useEffect(() => {
    const loadRecent = () => {
      const recentStr = localStorage.getItem('lanshare_recent_connections') || '[]';
      try {
        setRecentConnections(JSON.parse(recentStr));
      } catch {}
    };
    loadRecent();
  }, [tab, streamConnected]);

  // Real-time client logs state
  const [browserLogs, setBrowserLogs] = useState<string[]>([]);
  useEffect(() => {
    const originalLog = console.log;
    const originalWarn = console.warn;
    const originalError = console.error;

    const addLog = (type: string, args: any[]) => {
      const msg = args.map(arg => typeof arg === 'object' ? JSON.stringify(arg) : String(arg)).join(' ');
      setBrowserLogs(prev => [...prev.slice(-49), `[${type}] ${msg}`]);
    };

    console.log = (...args) => {
      addLog('LOG', args);
      originalLog.apply(console, args);
    };
    console.warn = (...args) => {
      addLog('WARN', args);
      originalWarn.apply(console, args);
    };
    console.error = (...args) => {
      addLog('ERROR', args);
      originalError.apply(console, args);
    };

    const handleWindowError = (e: ErrorEvent) => {
      addLog('FATAL', [e.message, e.filename, e.lineno]);
    };
    window.addEventListener('error', handleWindowError);

    return () => {
      console.log = originalLog;
      console.warn = originalWarn;
      console.error = originalError;
      window.removeEventListener('error', handleWindowError);
    };
  }, []);

  const [logs, setLogs] = useState<string[]>([]);
  const [logType, setLogType] = useState<string>('service');
  const [logSearch, setLogSearch] = useState('');
  const logsEndRef = useRef<HTMLDivElement>(null);

  // Performance history accumulator
  const [statsHistory, setStatsHistory] = useState<{ fps: number; decode: number; latency: number }[]>([]);
  const chartCanvasRef = useRef<HTMLCanvasElement>(null);
  const hudChartCanvasRef = useRef<HTMLCanvasElement>(null);
  const [showHudDiagnostics, setShowHudDiagnostics] = useState(false);
  const [showStreamSettings, setShowStreamSettings] = useState(false);
  const [targetFps, setTargetFps] = useState(60);
  const [targetScale, setTargetScale] = useState(1.0);
  const [targetBitrateMbps, setTargetBitrateMbps] = useState(4.0);

  const handleUpdateStreamSettings = (fps: number, scale: number, bitrateMbps: number) => {
    invoke('update_stream_settings', {
      fps,
      scale,
      bitrateBps: Math.round(bitrateMbps * 1_000_000)
    }).catch(err => {
      console.error("Failed to update stream settings:", err);
      addToast('Settings Error', 'Could not apply settings to host: ' + err, 'error');
    });
  };

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

  // Client-specific typed event listeners
  useEffect(() => {
    const unlistenVideoChunk = listen<{
      data: string;
      timestamp_us: number;
      is_keyframe: boolean;
      width: number;
      height: number;
      display_id: number;
    }>('video_chunk', (event) => {
      const dId = event.payload.display_id ?? 0;
      setActiveDisplays(prev => prev.includes(dId) ? prev : [...prev, dId].sort());
      feedChunk(
        dId,
        event.payload.data,
        event.payload.timestamp_us,
        event.payload.is_keyframe,
        event.payload.width,
        event.payload.height,
      );
    });

    const unlistenConnected = listen<{ host_addr: string; recv_port: number }>('stream_connected', () => {
      setStreamConnected(true);
      setTab('stream');
      setError(null);
      addToast('Stream Connected', 'Established direct connection with remote host.', 'success');

      // Save to recent connections
      const { connectedHost } = useSessionStore.getState();
      if (connectedHost) {
        const recentStr = localStorage.getItem('lanshare_recent_connections') || '[]';
        let recent: DiscoveredHost[] = [];
        try {
          recent = JSON.parse(recentStr);
        } catch {}
        recent = recent.filter(h => h.address !== connectedHost.address || h.port !== connectedHost.port);
        recent.unshift(connectedHost);
        recent = recent.slice(0, 5);
        localStorage.setItem('lanshare_recent_connections', JSON.stringify(recent));
      }
    });

    const unlistenDisconnected = listen<{ reason: string }>('stream_disconnected', () => {
      setStreamConnected(false);
      setTab('join');
      closeAllDecoders();
      setActiveDisplays([0]);
      setActiveDisplayTab(0);
      disconnectFromHost();
      addToast('Stream Disconnected', 'Connection with host terminated.', 'info');
    });

    const unlistenCursorChanged = listen<{ shape: string }>('cursor_changed', (event) => {
      setCursorShape(event.payload.shape);
    });

    const unlistenRecvStats = listen<{
      fps: number;
      packet_loss_pct: number;
      rtt_ms: number;
      bitrate_kbps: number;
    }>('recv_stats', (event) => {
      setPacketLossPct(event.payload.packet_loss_pct);
      useSessionStore.getState().updateStats({
        fps: event.payload.fps,
        encode_ms: 0,
        latency_ms: event.payload.rtt_ms,
        bitrate_kbps: event.payload.bitrate_kbps,
        client_count: 0,
        gpu_path_active: false,
      });
    });

    const unlistenMetrics = listen<{
      process_memory_bytes: number;
    }>('metrics_update', (event) => {
      setPlayerMemoryBytes(event.payload.process_memory_bytes);
    });

    return () => {
      unlistenVideoChunk.then(f => f());
      unlistenConnected.then(f => f());
      unlistenDisconnected.then(f => f());
      unlistenCursorChanged.then(f => f());
      unlistenRecvStats.then(f => f());
      unlistenMetrics.then(f => f());
    };
  }, [feedChunk, closeAllDecoders, addToast, disconnectFromHost]);

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
    if (tab !== 'logs') return;
    fetchLogs();
    const iv = setInterval(fetchLogs, 1500);
    return () => clearInterval(iv);
  }, [fetchLogs, tab]);

  // Scroll logs to bottom
  useEffect(() => {
    if (logsEndRef.current) {
      logsEndRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [logs]);

  // Poll host processes when on remote manager tab
  useEffect(() => {
    if (tab !== 'remote_manager' || !streamConnected) return;
    fetchHostProcesses();
    const iv = setInterval(fetchHostProcesses, 5000);
    return () => clearInterval(iv);
  }, [fetchHostProcesses, tab, streamConnected]);

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

  // Render HUD mini diagnostics graph
  useEffect(() => {
    const canvas = hudChartCanvasRef.current;
    if (!canvas || statsHistory.length < 2) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);

    // Mini Grid lines
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.04)';
    ctx.lineWidth = 1;
    for (let i = 1; i < 3; i++) {
      const y = (h / 3) * i;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
    }

    // Draw Decode FPS (Green)
    ctx.beginPath();
    ctx.strokeStyle = '#10b981';
    ctx.lineWidth = 1.5;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.fps, 75) / 75) * (h - 12) - 6;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();

    // Draw Decode Delay (Purple)
    ctx.beginPath();
    ctx.strokeStyle = '#818cf8';
    ctx.lineWidth = 1;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.decode, 20) / 20) * (h - 12) - 6;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();

    // Draw RTT Network Latency (Blue)
    ctx.beginPath();
    ctx.strokeStyle = '#3b82f6';
    ctx.lineWidth = 1;
    statsHistory.forEach((item, index) => {
      const x = (index / (statsHistory.length - 1)) * w;
      const y = h - (Math.min(item.latency, 40) / 40) * (h - 12) - 6;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
  }, [statsHistory, showHudDiagnostics]);

  // Input Forwarding event handlers
  const getRelativeMouseCoords = (e: React.MouseEvent<HTMLCanvasElement>, displayId?: number) => {
    const dId = displayId ?? 0;
    const canvas = document.getElementById(`stream-canvas-${dId}`) as HTMLCanvasElement;
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();

    const videoWidth = canvas.width;
    const videoHeight = canvas.height;
    if (videoWidth === 0 || videoHeight === 0) return null;

    if (scalingMode === 'stretch') {
      const xCanvas = e.clientX - rect.left;
      const yCanvas = e.clientY - rect.top;
      return {
        x: xCanvas,
        y: yCanvas,
        w: Math.round(rect.width),
        h: Math.round(rect.height),
      };
    }

    if (scalingMode === 'original') {
      const xCanvas = e.clientX - rect.left;
      const yCanvas = e.clientY - rect.top;
      return {
        x: xCanvas,
        y: yCanvas,
        w: videoWidth,
        h: videoHeight,
      };
    }

    // Default: 'fit' (contain)
    const videoRatio = videoWidth / videoHeight;
    const displayRatio = rect.width / rect.height;

    let renderedWidth = rect.width;
    let renderedHeight = rect.height;
    let leftOffset = 0;
    let topOffset = 0;

    if (displayRatio > videoRatio) {
      // Height-constrained (pillarbox, black bars on left/right)
      renderedWidth = rect.height * videoRatio;
      leftOffset = (rect.width - renderedWidth) / 2;
    } else {
      // Width-constrained (letterbox, black bars on top/bottom)
      renderedHeight = rect.width / videoRatio;
      topOffset = (rect.height - renderedHeight) / 2;
    }

    const xCanvas = e.clientX - rect.left;
    const yCanvas = e.clientY - rect.top;

    const xVideo = Math.max(0, Math.min(renderedWidth, xCanvas - leftOffset));
    const yVideo = Math.max(0, Math.min(renderedHeight, yCanvas - topOffset));

    return {
      x: xVideo,
      y: yVideo,
      w: Math.round(renderedWidth),
      h: Math.round(renderedHeight),
    };
  };

  const handleMouseMove = (e: React.MouseEvent<HTMLCanvasElement>, displayId?: number) => {
    if (!streamConnected) return;
    const coords = getRelativeMouseCoords(e, displayId);
    if (!coords) return;
    invoke('send_input', {
      event: {
        kind: 'mouse_move',
        x: coords.x,
        y: coords.y,
        viewport_w: coords.w,
        viewport_h: coords.h,
        display_id: displayId,
      }
    }).catch(console.error);
  };

  const handleMouseButton = (e: React.MouseEvent<HTMLCanvasElement>, pressed: boolean, displayId?: number) => {
    if (!streamConnected) return;
    const coords = getRelativeMouseCoords(e, displayId);
    if (!coords) return;

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
        x: coords.x,
        y: coords.y,
        viewport_w: coords.w,
        viewport_h: coords.h,
        display_id: displayId,
      }
    }).catch(console.error);
  };

  const handleWheel = (e: React.WheelEvent<HTMLCanvasElement>, displayId?: number) => {
    if (!streamConnected) return;
    const deltaY = -Math.sign(e.deltaY);
    invoke('send_input', {
      event: {
        kind: 'mouse_scroll',
        delta_x: 0.0,
        delta_y: deltaY,
        display_id: displayId
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

      const keyInfo = KEY_MAP[e.code] || { scan: 0, extended: false };
      pressedKeysRef.current.set(e.code, {
        keyCode: e.keyCode,
        scan: keyInfo.scan,
        extended: keyInfo.extended || false
      });

      invoke('send_input', {
        event: {
          kind: 'key_press',
          vk_code: e.keyCode,
          scan_code: keyInfo.scan,
          pressed: true,
          is_extended: keyInfo.extended || false
        }
      }).catch(console.error);
    };

    const handleKeyUp = (e: KeyboardEvent) => {
      const keysToPrevent = ['Backspace', 'Tab', 'Space', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
      if (keysToPrevent.includes(e.code) || e.key === ' ') {
        e.preventDefault();
      }

      const keyInfo = KEY_MAP[e.code] || { scan: 0, extended: false };
      pressedKeysRef.current.delete(e.code);

      invoke('send_input', {
        event: {
          kind: 'key_press',
          vk_code: e.keyCode,
          scan_code: keyInfo.scan,
          pressed: false,
          is_extended: keyInfo.extended || false
        }
      }).catch(console.error);
    };

    const handleBlur = () => {
      pressedKeysRef.current.forEach((val) => {
        invoke('send_input', {
          event: {
            kind: 'key_press',
            vk_code: val.keyCode,
            scan_code: val.scan,
            pressed: false,
            is_extended: val.extended
          }
        }).catch(console.error);
      });
      pressedKeysRef.current.clear();
    };

    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('keyup', handleKeyUp);
    window.addEventListener('blur', handleBlur);
    return () => {
      pressedKeysRef.current.clear();
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('keyup', handleKeyUp);
      window.removeEventListener('blur', handleBlur);
    };
  }, [streamConnected]);

  // Cleanup decoder on unmount to prevent WebCodecs resource leak
  useEffect(() => {
    return () => {
      closeAllDecoders();
    };
  }, [closeAllDecoders]);

  // Fullscreen toggle
  const toggleFullscreen = () => {
    const activeStatsId = typeof activeDisplayTab === 'number' ? activeDisplayTab : 0;
    const canvas = document.getElementById(`stream-canvas-${activeStatsId}`);
    const el = canvas?.parentElement;
    if (!el) return;
    if (!document.fullscreenElement) {
      el.requestFullscreen().then(() => setFullscreen(true));
    } else {
      document.exitFullscreen().then(() => setFullscreen(false));
    }
  };

  const handleSendWol = async () => {
    if (!wolMac) return;
    setWolSending(true);
    setWolStatus('');
    try {
      await invoke('send_wol_packet', { mac: wolMac });
      localStorage.setItem('lanshare_last_mac', wolMac);
      setWolStatus('Wake magic packet sent!');
      addToast('Wake magic packet sent successfully!', 'success');
    } catch (e: any) {
      setWolStatus(`Error: ${e}`);
      addToast(`Failed to send wake packet: ${e}`, 'error');
    } finally {
      setWolSending(false);
    }
  };

  const handleConnect = async (host: DiscoveredHost, skipPairingCheck = false) => {
    if (host.mac) {
      setWolMac(host.mac);
      localStorage.setItem('lanshare_last_mac', host.mac);
    }
    if (!skipPairingCheck && pairingInput && pairingInput.length < 6) {
      setError('Pairing code or password must be at least 6 characters.');
      return;
    }
    setError(null);
    setConnectingHost(host);

    try {
      await connectToHost(host, pairingInput);
      // State transitions (streamConnected, tab switch) are handled by
      // the stream_connected event listener — no optimistic setting.
    } catch {
      setStreamConnected(false);
      setError('Connection refused. Please confirm code expiration.');
      addToast('Connection Failed', 'Unable to reach the host session.', 'error');
    }
    setConnectingHost(null);
  };

  const handleDisconnect = async () => {
    closeAllDecoders();
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

  const sendFile = async (file: File) => {
    setFileTransferring(file.name);
    setFileProgress(0);
    try {
      await invoke('send_file_start', { name: file.name, size: file.size });
      
      const chunkSize = 64 * 1024; // 64 KB chunks
      let offset = 0;
      
      while (offset < file.size) {
        const slice = file.slice(offset, offset + chunkSize);
        const arrayBuffer = await slice.arrayBuffer();
        
        const bytes = new Uint8Array(arrayBuffer);
        let binary = '';
        const len = bytes.byteLength;
        const subChunkSize = 8192;
        for (let i = 0; i < len; i += subChunkSize) {
          binary += String.fromCharCode.apply(
            null,
            bytes.subarray(i, i + subChunkSize) as unknown as number[]
          );
        }
        const base64Data = btoa(binary);
        
        await invoke('send_file_chunk', { data: base64Data });
        offset += chunkSize;
        setFileProgress(Math.min(100, Math.round((offset / file.size) * 100)));
      }
      
      await invoke('send_file_end');
      addToast('File Sent', `Successfully transferred ${file.name} to host Downloads.`, 'success');
    } catch (e) {
      console.error('File transfer failed:', e);
      addToast('File Transfer Failed', `Error sending ${file.name}: ${e}`, 'error');
    } finally {
      setFileTransferring(null);
    }
  };

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
  };

  const handleDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      const file = e.dataTransfer.files[0];
      sendFile(file);
    }
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

          <div 
            className={`sidebar-item ${tab === 'remote_manager' ? 'active' : ''} ${!streamConnected ? 'disabled' : ''}`}
            onClick={() => streamConnected && setTab('remote_manager')}
            style={{ opacity: streamConnected ? 1 : 0.45, cursor: streamConnected ? 'pointer' : 'not-allowed' }}
          >
            <Cpu size={16} />
            <span>Remote Manager</span>
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
            {tab === 'join' ? 'Connect to Stream' : tab === 'stream' ? 'Video Stream HUD' : tab === 'performance' ? 'Decoding Latency Graph' : tab === 'remote_manager' ? 'Remote Task Manager' : 'System Logs'}
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

              {/* Recent Connections */}
              {recentConnections.length > 0 && (
                <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                    <History size={16} style={{ color: 'var(--accent-purple)' }} />
                    <h3>Recent Connections</h3>
                  </div>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
                    {recentConnections.map(host => (
                      <div key={`${host.address}:${host.port}`} className="device-card">
                        <div style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
                          <Wifi size={18} style={{ color: 'var(--text-muted)' }} />
                          <div>
                            <div style={{ fontWeight: 600, fontSize: '0.9rem', color: '#fff' }}>{host.name}</div>
                            <div style={{ fontSize: '0.78rem', color: 'var(--text-secondary)', marginTop: '2px' }}>
                              IP Target: {host.address}:{host.port}
                            </div>
                          </div>
                        </div>
                        <div style={{ display: 'flex', gap: '8px' }}>
                          <button
                            id={`btn-connect-recent-${host.address.replace(/\./g, '-')}`}
                            className="btn btn-primary btn-sm"
                            onClick={() => handleConnect(host)}
                            disabled={connectingHost?.address === host.address}
                          >
                            {connectingHost?.address === host.address ? <div className="spinner" /> : 'Connect'}
                          </button>
                          <button
                            className="btn btn-ghost btn-sm"
                            onClick={() => {
                              const updated = recentConnections.filter(h => h.address !== host.address || h.port !== host.port);
                              localStorage.setItem('lanshare_recent_connections', JSON.stringify(updated));
                              setRecentConnections(updated);
                            }}
                            style={{ color: 'var(--danger)' }}
                          >
                            <Trash2 size={14} />
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* Pairing code inputs */}
              <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                <h3>Connection Pairing Code</h3>
                <input
                  id="pairing-code-input"
                  type="text"
                  placeholder="Enter pairing code or unattended password"
                  maxLength={32}
                  value={pairingInput}
                  onChange={e => setPairingInput(e.target.value)}
                  style={{ fontSize: '1.4rem', letterSpacing: '0.25em', textAlign: 'center', fontFamily: 'monospace' }}
                />
                {error && <p style={{ color: 'var(--danger)', fontSize: '0.8rem' }}>{error}</p>}
              </div>

              {/* Wake-on-LAN (WoL) */}
              <div className="card" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                <h3>Wake-on-LAN (WoL)</h3>
                <div style={{ display: 'flex', gap: '10px' }}>
                  <input
                    id="wol-mac-input"
                    placeholder="MAC Address (e.g. 00:11:22:33:44:55)"
                    value={wolMac}
                    onChange={e => setWolMac(e.target.value)}
                    style={{ flex: 1 }}
                  />
                  <button
                    id="btn-send-wol"
                    className="btn btn-secondary btn-sm"
                    onClick={handleSendWol}
                    disabled={wolSending}
                  >
                    {wolSending ? 'Sending...' : 'Wake'}
                  </button>
                </div>
                {wolStatus && <p style={{ fontSize: '0.78rem', color: 'var(--text-secondary)' }}>{wolStatus}</p>}
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
            <div style={{ display: 'flex', flexDirection: 'column', width: '100%', height: 'calc(100vh - 120px)', background: '#000', position: 'relative' }}>
              
              {/* Display Tab Selector */}
              {activeDisplays.length > 1 && (
                <div style={{
                  display: 'flex',
                  gap: '8px',
                  padding: '10px 16px',
                  background: 'rgba(10, 8, 20, 0.85)',
                  borderBottom: '1px solid var(--border)',
                  backdropFilter: 'blur(12px)',
                  zIndex: 10
                }}>
                  {activeDisplays.map(dId => (
                    <button
                      key={dId}
                      className={`btn btn-sm ${activeDisplayTab === dId ? 'btn-primary' : 'btn-ghost'}`}
                      onClick={() => setActiveDisplayTab(dId)}
                      style={{ padding: '6px 12px', fontSize: '0.8rem', fontWeight: 600 }}
                    >
                      🖥️ Screen {dId + 1}
                    </button>
                  ))}
                  <button
                    className={`btn btn-sm ${activeDisplayTab === 'grid' ? 'btn-primary' : 'btn-ghost'}`}
                    onClick={() => setActiveDisplayTab('grid')}
                    style={{ padding: '6px 12px', fontSize: '0.8rem', fontWeight: 600 }}
                  >
                    🥞 Split Grid
                  </button>
                </div>
              )}

              {/* HTML5 Video Decoder Canvas Viewport(s) */}
              <div style={{
                flex: 1,
                width: '100%',
                position: 'relative',
                overflow: activeDisplayTab === 'grid' ? 'auto' : (scalingMode === 'original' ? 'auto' : 'hidden')
              }}>
                <div style={{
                  display: 'grid',
                  gridTemplateColumns: activeDisplays.length > 1 && activeDisplayTab === 'grid' ? 'repeat(auto-fit, minmax(640px, 1fr))' : '1fr',
                  gap: '16px',
                  width: '100%',
                  height: activeDisplays.length > 1 && activeDisplayTab === 'grid' ? 'auto' : '100%',
                  padding: activeDisplays.length > 1 && activeDisplayTab === 'grid' ? '20px' : '0',
                  boxSizing: 'border-box'
                }}>
                  {activeDisplays.map(dId => {
                    const isVisible = activeDisplayTab === 'grid' || activeDisplayTab === dId;
                    if (!isVisible) return null;

                    const canvasWidth = document.getElementById(`stream-canvas-${dId}`)?.getAttribute('width') || '1920';
                    const canvasHeight = document.getElementById(`stream-canvas-${dId}`)?.getAttribute('height') || '1080';

                    return (
                      <div
                        key={dId}
                        style={{
                          position: 'relative',
                          display: 'flex',
                          flexDirection: 'column',
                          height: activeDisplays.length > 1 && activeDisplayTab === 'grid' ? '420px' : '100%',
                          border: activeDisplays.length > 1 && activeDisplayTab === 'grid' ? '1px solid var(--border)' : 'none',
                          borderRadius: activeDisplays.length > 1 && activeDisplayTab === 'grid' ? 'var(--radius-lg)' : '0',
                          overflow: 'hidden',
                          background: '#000'
                        }}
                      >
                        {activeDisplays.length > 1 && activeDisplayTab === 'grid' && (
                          <div style={{
                            padding: '6px 12px',
                            background: 'rgba(0,0,0,0.5)',
                            borderBottom: '1px solid var(--border)',
                            fontSize: '0.75rem',
                            fontWeight: 600,
                            color: 'var(--text-secondary)',
                            display: 'flex',
                            justifyContent: 'space-between'
                          }}>
                            <span>Display {dId + 1}</span>
                            {displayStats[dId] && (
                              <span>{displayStats[dId].fps} FPS</span>
                            )}
                          </div>
                        )}
                        <div style={{ flex: 1, position: 'relative', height: '100%' }}>
                          <canvas
                            ref={(el) => registerCanvas(dId, el)}
                            id={`stream-canvas-${dId}`}
                            style={{
                              width: scalingMode === 'original' && activeDisplayTab !== 'grid' ? `${canvasWidth}px` : '100%',
                              height: scalingMode === 'original' && activeDisplayTab !== 'grid' ? `${canvasHeight}px` : '100%',
                              objectFit: scalingMode === 'fit' ? 'contain' : scalingMode === 'stretch' ? 'fill' : 'none',
                              display: 'block',
                              cursor: cursorShape
                            }}
                            onMouseMove={(e) => handleMouseMove(e, dId)}
                            onMouseDown={(e) => handleMouseButton(e, true, dId)}
                            onMouseUp={(e) => handleMouseButton(e, false, dId)}
                            onWheel={(e) => handleWheel(e, dId)}
                            onContextMenu={(e) => e.preventDefault()}
                            onDragOver={handleDragOver}
                            onDrop={handleDrop}
                          />
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>

              {/* File Transfer Progress Overlay */}
              {fileTransferring && (
                <div style={{
                  position: 'absolute', top: 0, left: 0, right: 0, bottom: 0,
                  background: 'rgba(10, 8, 20, 0.75)', backdropFilter: 'blur(8px)',
                  display: 'flex', flexDirection: 'column', justifyContent: 'center', alignItems: 'center',
                  color: '#fff', gap: '16px', zIndex: 20
                }}>
                  <div className="card" style={{ width: '320px', padding: '24px', textAlign: 'center', border: '1px solid var(--border)' }}>
                    <div style={{ fontWeight: 600, fontSize: '0.95rem', marginBottom: '8px' }}>
                      Transferring File...
                    </div>
                    <div style={{ fontSize: '0.8rem', color: 'var(--text-secondary)', marginBottom: '16px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {fileTransferring}
                    </div>
                    {/* Progress Bar */}
                    <div style={{ width: '100%', height: '8px', background: 'rgba(255,255,255,0.08)', borderRadius: '4px', overflow: 'hidden', marginBottom: '8px' }}>
                      <div style={{ width: `${fileProgress}%`, height: '100%', background: 'linear-gradient(90deg, var(--accent) 0%, var(--accent-purple) 100%)', transition: 'width 0.1s ease-out' }} />
                    </div>
                    <div style={{ fontSize: '0.78rem', fontFamily: 'monospace', color: 'var(--accent)' }}>
                      {fileProgress}%
                    </div>
                  </div>
                </div>
              )}

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
                  {isRecording ? (
                    <div style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--danger)' }}>
                      <div className="pulse-dot" style={{ background: 'var(--danger)', boxShadow: '0 0 8px var(--danger)', animation: 'pulse-danger 2s ease-in-out infinite' }} />
                      <span style={{ fontWeight: 600, fontFamily: 'monospace' }}>REC {formatTime(recordingTime)}</span>
                    </div>
                  ) : (
                    <Activity size={14} style={{ color: '#10b981' }} />
                  )}
                  <span style={{ fontFamily: 'monospace' }}>FPS: {decodeStats.fps}</span>
                  <span style={{ fontFamily: 'monospace' }}>RTT: {sessionStats.latency_ms}ms</span>
                  <span style={{ fontFamily: 'monospace' }}>Bitrate: {(sessionStats.bitrate_kbps / 1000).toFixed(1)} Mbps</span>
                  <span style={{ fontFamily: 'monospace' }}>IDR: {decodeStats.keyframes}</span>
                </div>

                {/* HUD controls (clickable) */}
                <div style={{
                  display: 'flex', gap: '10px', alignItems: 'center', pointerEvents: 'auto'
                }}>
                  {/* Scaling Mode Selector */}
                  <div style={{
                    display: 'flex', background: 'rgba(10, 8, 20, 0.7)', borderRadius: 'var(--radius-sm)',
                    border: '1px solid var(--border)', backdropFilter: 'var(--glass-blur)', overflow: 'hidden',
                    height: '32px', alignItems: 'center'
                  }}>
                    {(['fit', 'stretch', 'original'] as const).map(mode => (
                      <button
                        key={mode}
                        className={`btn ${scalingMode === mode ? 'btn-primary' : 'btn-ghost'}`}
                        onClick={() => setScalingMode(mode)}
                        style={{
                          textTransform: 'capitalize', padding: '0 8px', fontSize: '0.72rem',
                          borderRadius: 0, border: 'none', height: '100%', minHeight: 'unset',
                          fontWeight: 500
                        }}
                      >
                        {mode}
                      </button>
                    ))}
                  </div>

                  <button 
                    className={`btn ${isRecording ? 'btn-danger' : 'btn-ghost'} btn-sm`}
                    onClick={isRecording ? stopRecording : startRecording}
                    style={{ height: '32px', background: isRecording ? undefined : 'rgba(10, 8, 20, 0.7)', backdropFilter: isRecording ? undefined : 'var(--glass-blur)' }}
                  >
                    {isRecording ? 'Stop REC' : 'Record'}
                  </button>

                   <button 
                    className={`btn ${showHudDiagnostics ? 'btn-primary' : 'btn-ghost'} btn-sm`}
                    onClick={() => {
                      setShowHudDiagnostics(!showHudDiagnostics);
                      if (showStreamSettings) setShowStreamSettings(false);
                    }}
                    style={{ height: '32px', background: showHudDiagnostics ? undefined : 'rgba(10, 8, 20, 0.7)', backdropFilter: showHudDiagnostics ? undefined : 'var(--glass-blur)' }}
                  >
                    Diagnostics
                  </button>

                  <button 
                    className={`btn ${showStreamSettings ? 'btn-primary' : 'btn-ghost'} btn-sm`}
                    onClick={() => {
                      setShowStreamSettings(!showStreamSettings);
                      if (showHudDiagnostics) setShowHudDiagnostics(false);
                    }}
                    style={{ height: '32px', background: showStreamSettings ? undefined : 'rgba(10, 8, 20, 0.7)', backdropFilter: showStreamSettings ? undefined : 'var(--glass-blur)' }}
                  >
                    Stream Settings
                  </button>

                  <button 
                    className="btn btn-ghost btn-sm"
                    onClick={() => invoke('request_keyframe').then(() => addToast('Keyframe Requested', 'Sent IDR request to host.', 'info')).catch(console.error)}
                    style={{ background: 'rgba(10, 8, 20, 0.7)', backdropFilter: 'var(--glass-blur)', height: '32px' }}
                  >
                    Request IDR
                  </button>
                  
                  <button 
                    className="btn btn-ghost btn-sm"
                    onClick={toggleFullscreen}
                    style={{ background: 'rgba(10, 8, 20, 0.7)', backdropFilter: 'var(--glass-blur)', height: '32px' }}
                  >
                    {fullscreen ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
                  </button>
                  
                  <button 
                    className="btn btn-danger btn-sm"
                    onClick={handleDisconnect}
                    style={{ height: '32px' }}
                  >
                    Disconnect
                  </button>
                </div>
              </div>

              {showStreamSettings && (
                <div style={{
                  position: 'absolute', top: '64px', right: '16px',
                  background: 'rgba(10, 8, 20, 0.85)', borderRadius: 'var(--radius-md)',
                  border: '1px solid var(--border)', padding: '14px 16px',
                  display: 'flex', flexDirection: 'column', gap: '14px',
                  width: '280px', pointerEvents: 'auto', zIndex: 10,
                  backdropFilter: 'var(--glass-blur)', color: '#fff',
                  boxShadow: 'var(--shadow-lg)'
                }}>
                  <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', borderBottom: '1px solid rgba(255,255,255,0.1)', paddingBottom: '6px' }}>
                    <span style={{ fontWeight: 600, fontSize: '0.8rem', color: '#fff' }}>Stream Settings</span>
                    <button 
                      className="btn btn-ghost btn-xs" 
                      onClick={() => setShowStreamSettings(false)} 
                      style={{ minHeight: 'unset', height: '18px', padding: '0 4px', color: 'var(--text-secondary)' }}
                    >
                      ✕
                    </button>
                  </div>

                  <div>
                    <label style={{ fontSize: '0.74rem', color: '#ccc', display: 'block', marginBottom: '6px' }}>
                      Target FPS: <strong style={{ color: '#fff', fontFamily: 'monospace' }}>{targetFps} FPS</strong>
                    </label>
                    <input
                      type="range"
                      min={15} max={60} step={5}
                      value={targetFps}
                      onChange={e => {
                        const v = Number(e.target.value);
                        setTargetFps(v);
                        handleUpdateStreamSettings(v, targetScale, targetBitrateMbps);
                      }}
                      style={{ width: '100%', display: 'block', accentColor: 'var(--primary)' }}
                    />
                  </div>

                  <div>
                    <label style={{ fontSize: '0.74rem', color: '#ccc', display: 'block', marginBottom: '6px' }}>
                      Resolution Scale: <strong style={{ color: '#fff', fontFamily: 'monospace' }}>{Math.round(targetScale * 100)}%</strong>
                    </label>
                    <input
                      type="range"
                      min={0.25} max={1.0} step={0.25}
                      value={targetScale}
                      onChange={e => {
                        const v = Number(e.target.value);
                        setTargetScale(v);
                        handleUpdateStreamSettings(targetFps, v, targetBitrateMbps);
                      }}
                      style={{ width: '100%', display: 'block', accentColor: 'var(--primary)' }}
                    />
                  </div>

                  <div>
                    <label style={{ fontSize: '0.74rem', color: '#ccc', display: 'block', marginBottom: '6px' }}>
                      Max Bitrate: <strong style={{ color: '#fff', fontFamily: 'monospace' }}>{targetBitrateMbps.toFixed(1)} Mbps</strong>
                    </label>
                    <input
                      type="range"
                      min={0.5} max={30.0} step={0.5}
                      value={targetBitrateMbps}
                      onChange={e => {
                        const v = Number(e.target.value);
                        setTargetBitrateMbps(v);
                        handleUpdateStreamSettings(targetFps, targetScale, v);
                      }}
                      style={{ width: '100%', display: 'block', accentColor: 'var(--primary)' }}
                    />
                  </div>
                </div>
              )}

              {showHudDiagnostics && (
                <div style={{
                  position: 'absolute', top: '64px', right: '16px',
                  background: 'rgba(10, 8, 20, 0.85)', borderRadius: 'var(--radius-md)',
                  border: '1px solid var(--border)', padding: '12px 16px',
                  display: 'flex', flexDirection: 'column', gap: '10px',
                  width: '280px', pointerEvents: 'auto', zIndex: 10,
                  backdropFilter: 'var(--glass-blur)',
                }}>
                  <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                    <span style={{ fontWeight: 600, fontSize: '0.8rem', color: '#fff' }}>Real-Time Diagnostics</span>
                    <button 
                      className="btn btn-ghost btn-xs" 
                      onClick={() => setShowHudDiagnostics(false)} 
                      style={{ minHeight: 'unset', height: '18px', padding: '0 4px', color: 'var(--text-secondary)' }}
                    >
                      ✕
                    </button>
                  </div>
                  <canvas ref={hudChartCanvasRef} width={250} height={100} style={{ width: '100%', height: '100px', display: 'block', background: 'rgba(0,0,0,0.2)', borderRadius: 'var(--radius-sm)' }} />
                  <div style={{ display: 'flex', justifyContent: 'space-between', flexWrap: 'wrap', gap: '4px', fontSize: '0.68rem' }}>
                    <span style={{ color: '#10b981', display: 'flex', alignItems: 'center', gap: '2px' }}>■ FPS</span>
                    <span style={{ color: '#818cf8', display: 'flex', alignItems: 'center', gap: '2px' }}>■ Delay</span>
                    <span style={{ color: '#3b82f6', display: 'flex', alignItems: 'center', gap: '2px' }}>■ Ping</span>
                  </div>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '4px', fontSize: '0.72rem', color: 'var(--text-secondary)', borderTop: '1px solid rgba(255,255,255,0.05)', paddingTop: '6px' }}>
                    <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                      <span>Packets Loss:</span>
                      <span style={{ color: packetLossPct > 2 ? 'var(--danger)' : '#fff', fontFamily: 'monospace' }}>
                        {packetLossPct?.toFixed(1) || '0.0'}%
                      </span>
                    </div>
                    <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                      <span>Client Memory:</span>
                      <span style={{ color: '#fff', fontFamily: 'monospace' }}>
                        {playerMemoryBytes ? `${(playerMemoryBytes / 1024 / 1024).toFixed(1)} MB` : '—'}
                      </span>
                    </div>
                  </div>
                </div>
              )}

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

          {/* TAB 5: Remote Task Manager & File Browser */}
          {tab === 'remote_manager' && streamConnected && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '16px', height: '100%' }}>
              
              {/* Segmented Picker for Remote Manager Sub-Tabs */}
              <div style={{
                display: 'flex',
                background: 'rgba(255, 255, 255, 0.03)',
                border: '1px solid var(--border)',
                borderRadius: 'var(--radius-md)',
                padding: '4px',
                gap: '4px',
                width: '320px'
              }}>
                <button
                  className={`btn btn-sm ${remoteSubTab === 'processes' ? 'btn-primary' : 'btn-ghost'}`}
                  style={{ flex: 1, border: 'none', padding: '6px 12px' }}
                  onClick={() => {
                    setRemoteSubTab('processes');
                    fetchHostProcesses();
                  }}
                >
                  Task Manager
                </button>
                <button
                  className={`btn btn-sm ${remoteSubTab === 'files' ? 'btn-primary' : 'btn-ghost'}`}
                  style={{ flex: 1, border: 'none', padding: '6px 12px' }}
                  onClick={() => setRemoteSubTab('files')}
                >
                  File Browser
                </button>
              </div>

              {remoteSubTab === 'files' ? (
                <FileManager />
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: '20px' }}>
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '12px' }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                  <RefreshCw 
                    size={16} 
                    className={processesLoading ? 'spinner' : ''} 
                    style={{ cursor: 'pointer', color: 'var(--accent)' }} 
                    onClick={fetchHostProcesses} 
                  />
                  <h3>Remote Host Processes</h3>
                </div>
                <input
                  type="text"
                  placeholder="Search processes by name..."
                  value={processSearch}
                  onChange={e => setProcessSearch(e.target.value)}
                  style={{ maxWidth: '300px' }}
                />
              </div>

              {processesLoading && hostProcesses.length === 0 ? (
                <div className="empty-state">
                  <div className="spinner" />
                  <p>Retrieving host process list...</p>
                </div>
              ) : hostProcesses.length === 0 ? (
                <div className="empty-state">
                  <p>No processes found.</p>
                </div>
              ) : (
                <div className="card" style={{ padding: '0', overflow: 'hidden', border: '1px solid var(--border)' }}>
                  <div style={{ maxHeight: 'calc(100vh - 280px)', overflowY: 'auto' }}>
                    <table style={{ width: '100%', borderCollapse: 'collapse', textAlign: 'left', fontSize: '0.88rem' }}>
                      <thead>
                        <tr style={{ background: 'rgba(255,255,255,0.02)', borderBottom: '1px solid var(--border)' }}>
                          <th style={{ padding: '12px 16px', fontWeight: 600, color: 'var(--text-secondary)' }}>Process Name</th>
                          <th style={{ padding: '12px 16px', fontWeight: 600, color: 'var(--text-secondary)' }}>PID</th>
                          <th style={{ padding: '12px 16px', fontWeight: 600, color: 'var(--text-secondary)' }}>Threads</th>
                          <th style={{ padding: '12px 16px', fontWeight: 600, color: 'var(--text-secondary)', textAlign: 'right' }}>Actions</th>
                        </tr>
                      </thead>
                      <tbody>
                        {hostProcesses
                          .filter(p => p.name.toLowerCase().includes(processSearch.toLowerCase()))
                          .sort((a, b) => a.name.localeCompare(b.name))
                          .map(proc => (
                            <tr key={proc.pid} style={{ borderBottom: '1px solid rgba(255,255,255,0.04)', transition: 'background 0.2s' }}>
                              <td style={{ padding: '10px 16px', fontWeight: 500, color: '#fff' }}>{proc.name}</td>
                              <td style={{ padding: '10px 16px', color: 'var(--text-secondary)', fontFamily: 'monospace' }}>{proc.pid}</td>
                              <td style={{ padding: '10px 16px', color: 'var(--text-secondary)' }}>{proc.threads}</td>
                              <td style={{ padding: '10px 16px', textAlign: 'right' }}>
                                <button
                                  className="btn btn-sm btn-ghost"
                                  style={{ color: 'var(--danger)', padding: '4px 8px', minHeight: 'unset' }}
                                  onClick={() => {
                                    setKillConfirmPid(proc.pid);
                                    setKillConfirmName(proc.name);
                                  }}
                                >
                                  End Task
                                </button>
                              </td>
                            </tr>
                          ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              )}

              {/* Confirmation Modal */}
              {killConfirmPid !== null && (
                <div style={{
                  position: 'fixed', top: 0, left: 0, right: 0, bottom: 0,
                  background: 'rgba(10, 8, 20, 0.75)', backdropFilter: 'blur(8px)',
                  display: 'flex', justifyContent: 'center', alignItems: 'center',
                  zIndex: 1000
                }}>
                  <div className="card" style={{ width: '400px', padding: '24px', display: 'flex', flexDirection: 'column', gap: '16px', border: '1px solid var(--border)' }}>
                    <h3 style={{ color: 'var(--danger)' }}>Confirm End Task</h3>
                    <p style={{ fontSize: '0.9rem', lineHeight: '1.4' }}>
                      Are you sure you want to terminate <strong>{killConfirmName}</strong> (PID: {killConfirmPid}) on the host machine?
                      Unsaved data in this process will be lost.
                    </p>
                    <div style={{ display: 'flex', justifyContent: 'flex-end', gap: '10px', marginTop: '10px' }}>
                      <button 
                        className="btn btn-ghost" 
                        onClick={() => {
                          setKillConfirmPid(null);
                          setKillConfirmName('');
                        }}
                      >
                        Cancel
                      </button>
                      <button 
                        className="btn btn-danger" 
                        onClick={() => {
                          killHostProcess(killConfirmPid);
                          setKillConfirmPid(null);
                          setKillConfirmName('');
                        }}
                      >
                        End Process
                      </button>
                    </div>
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}

          {/* TAB 4: Logs */}
          {tab === 'logs' && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '14px', height: '100%', minHeight: 'calc(100vh - 260px)' }}>
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '12px' }}>
                <div style={{ display: 'flex', gap: '8px' }}>
                  {['service', 'capture', 'network', 'metrics', 'browser'].map(type => (
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
                {logType === 'browser' ? (
                  browserLogs.length === 0 ? (
                    <div style={{ color: 'var(--text-muted)', textAlign: 'center', padding: '20px' }}>No browser console logs yet.</div>
                  ) : (
                    browserLogs
                      .filter(log => log.toLowerCase().includes(logSearch.toLowerCase()))
                      .map((log, index) => (
                        <div key={index} className={`log-entry ${log.startsWith('[ERROR]') || log.startsWith('[FATAL]') ? 'error' : log.startsWith('[WARN]') ? 'warn' : 'info'}`}>
                          <span className="log-text">{log}</span>
                        </div>
                      ))
                  )
                ) : logs.length === 0 ? (
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

      {/* Debug Overlay listener trigger */}
      {streamConnected && (
        <DebugOverlay 
          backend="WebCodecs" 
          sessionId={connectedHost ? connectedHost.name : "Pulse"} 
          clientStats={{
            decodeStats,
            sessionStats,
            packetLossPct,
            width: document.getElementById(`stream-canvas-${activeStatsId}`) ? (document.getElementById(`stream-canvas-${activeStatsId}`) as HTMLCanvasElement).width : 0,
            height: document.getElementById(`stream-canvas-${activeStatsId}`) ? (document.getElementById(`stream-canvas-${activeStatsId}`) as HTMLCanvasElement).height : 0
          }}
        />
      )}
    </div>
  );
};
