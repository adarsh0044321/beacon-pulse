import './style.css'

// ==========================================
// 1. AMBIENT BACKGROUND CANVAS SIMULATION
// ==========================================
const ambientCanvas = document.getElementById('ambient-canvas') as HTMLCanvasElement | null;
if (ambientCanvas) {
  const ctx = ambientCanvas.getContext('2d');
  
  const resizeCanvas = () => {
    ambientCanvas.width = window.innerWidth;
    ambientCanvas.height = window.innerHeight;
  };
  window.addEventListener('resize', resizeCanvas);
  resizeCanvas();

  interface Particle {
    x: number;
    y: number;
    vx: number;
    vy: number;
    radius: number;
    alpha: number;
  }

  const particles: Particle[] = [];
  const maxParticles = 40;
  
  for (let i = 0; i < maxParticles; i++) {
    particles.push({
      x: Math.random() * ambientCanvas.width,
      y: Math.random() * ambientCanvas.height,
      vx: (Math.random() - 0.5) * 0.3,
      vy: (Math.random() - 0.5) * 0.3,
      radius: Math.random() * 2 + 1,
      alpha: Math.random() * 0.3 + 0.1
    });
  }

  let mouseX = -1000;
  let mouseY = -1000;
  window.addEventListener('mousemove', (e) => {
    mouseX = e.clientX;
    mouseY = e.clientY;
  });

  const drawAmbient = () => {
    if (!ctx) return;
    ctx.clearRect(0, 0, ambientCanvas.width, ambientCanvas.height);
    
    for (let i = 0; i < maxParticles; i++) {
      const p1 = particles[i];
      p1.x += p1.vx;
      p1.y += p1.vy;

      if (p1.x < 0) p1.x = ambientCanvas.width;
      if (p1.x > ambientCanvas.width) p1.x = 0;
      if (p1.y < 0) p1.y = ambientCanvas.height;
      if (p1.y > ambientCanvas.height) p1.y = 0;

      ctx.beginPath();
      ctx.arc(p1.x, p1.y, p1.radius, 0, Math.PI * 2);
      ctx.fillStyle = `rgba(168, 85, 247, ${p1.alpha})`;
      ctx.fill();

      for (let j = i + 1; j < maxParticles; j++) {
        const p2 = particles[j];
        const dx = p1.x - p2.x;
        const dy = p1.y - p2.y;
        const dist = Math.sqrt(dx * dx + dy * dy);

        if (dist < 150) {
          ctx.beginPath();
          ctx.moveTo(p1.x, p1.y);
          ctx.lineTo(p2.x, p2.y);
          const edgeAlpha = (1 - dist / 150) * 0.12;
          ctx.strokeStyle = `rgba(6, 182, 212, ${edgeAlpha})`;
          ctx.lineWidth = 0.5;
          ctx.stroke();
        }
      }

      if (mouseX > 0 && mouseY > 0) {
        const dx = p1.x - mouseX;
        const dy = p1.y - mouseY;
        const dist = Math.sqrt(dx * dx + dy * dy);
        if (dist < 180) {
          const force = (180 - dist) / 180 * 0.3;
          p1.x += (dx / dist) * force;
          p1.y += (dy / dist) * force;
        }
      }
    }

    requestAnimationFrame(drawAmbient);
  };
  drawAmbient();
}


// ==========================================
// 2. WINDOWS MANAGEMENT LAYER (DRAG & DEPTH)
// ==========================================
let maxZIndex = 100;
const windowsList = document.querySelectorAll('.os-window') as NodeListOf<HTMLElement>;

function focusWindow(win: HTMLElement) {
  windowsList.forEach(w => {
    w.classList.remove('os-window-active');
  });
  
  win.classList.remove('hidden');
  win.classList.add('os-window-active');
  win.style.zIndex = (++maxZIndex).toString();
  
  // Update taskbar pills active visual indicator
  updateTaskbarPills();
}

// Drag and drop mechanics for window headers
windowsList.forEach(win => {
  const header = win.querySelector('.os-window-header') as HTMLElement | null;
  if (!header) return;

  header.addEventListener('mousedown', (e) => {
    if ((e.target as HTMLElement).tagName === 'BUTTON') return;
    
    focusWindow(win);
    
    const startX = e.clientX;
    const startY = e.clientY;
    
    // Parse current positioning offset
    const rect = win.getBoundingClientRect();
    const currentLeft = rect.left;
    const currentTop = rect.top;
    
    const onMouseMove = (moveEvent: MouseEvent) => {
      const deltaX = moveEvent.clientX - startX;
      const deltaY = moveEvent.clientY - startY;
      
      win.style.left = `${currentLeft + deltaX}px`;
      win.style.top = `${currentTop + deltaY}px`;
    };
    
    const onMouseUp = () => {
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
    };
    
    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  });

  // Make close/minimize buttons work
  const closeBtn = win.querySelector('.win-btn-close');
  closeBtn?.addEventListener('click', () => {
    win.classList.add('hidden');
    updateTaskbarPills();
  });

  const minimizeBtn = win.querySelector('.win-btn-minimize');
  minimizeBtn?.addEventListener('click', () => {
    win.classList.add('hidden');
    updateTaskbarPills();
  });

  // Bring to front on body mousedown click
  win.addEventListener('mousedown', () => {
    focusWindow(win);
  });
});

// Desktop icons double-click trigger
const desktopIcons = document.querySelectorAll('.os-icon-shortcut');
desktopIcons.forEach(icon => {
  icon.addEventListener('click', () => {
    const targetWinId = icon.getAttribute('data-open-win') || '';
    const targetWin = document.getElementById(targetWinId);
    if (targetWin) {
      focusWindow(targetWin);
    }
  });
});

// Taskbar pills click toggle actions
const taskbarPills = document.querySelectorAll('.task-pill') as NodeListOf<HTMLElement>;
taskbarPills.forEach(pill => {
  pill.addEventListener('click', () => {
    const targetWinId = pill.getAttribute('data-target-win') || '';
    const targetWin = document.getElementById(targetWinId);
    if (!targetWin) return;
    
    if (targetWin.classList.contains('hidden')) {
      focusWindow(targetWin);
    } else {
      if (targetWin.classList.contains('os-window-active')) {
        targetWin.classList.add('hidden');
        updateTaskbarPills();
      } else {
        focusWindow(targetWin);
      }
    }
  });
});

function updateTaskbarPills() {
  taskbarPills.forEach(pill => {
    const targetWinId = pill.getAttribute('data-target-win') || '';
    const targetWin = document.getElementById(targetWinId);
    
    if (!targetWin) return;
    
    if (targetWin.classList.contains('hidden')) {
      pill.className = 'task-pill px-3 py-1.5 rounded-lg bg-white/5 border border-white/5 text-slate-500 cursor-pointer hover:bg-white/10 active:scale-95 transition';
    } else {
      if (targetWin.classList.contains('os-window-active')) {
        pill.className = 'task-pill px-3 py-1.5 rounded-lg bg-purple-500/10 border border-purple-500/30 text-purple-300 font-bold cursor-pointer transition';
      } else {
        pill.className = 'task-pill px-3 py-1.5 rounded-lg bg-white/10 border border-white/10 text-slate-200 cursor-pointer hover:bg-white/15 transition';
      }
    }
  });
}
// Initial run
updateTaskbarPills();


// ==========================================
// 3. LIVE CLOCK SYSTEM TRAY WIDGET
// ==========================================
const clockTime = document.getElementById('os-clock-time');
if (clockTime) {
  const updateClock = () => {
    const now = new Date();
    const hour = now.getHours();
    const min = now.getMinutes().toString().padStart(2, '0');
    const ampm = hour >= 12 ? 'PM' : 'AM';
    const hour12 = hour % 12 || 12;
    clockTime.innerText = `${hour12}:${min} ${ampm}`;
  };
  setInterval(updateClock, 1000);
  updateClock();
}


// ==========================================
// 4. REMOTE WORKSPACE SIMULATOR CONTROLLERS
// ==========================================
let simStatus: 'offline' | 'scanning' | 'pairing' | 'connected' = 'offline';
let enteredCode = ['', '', '', '', '', ''];
const correctCode = '582914';

// Switch view nodes
const viewDisconnected = document.getElementById('sim-view-disconnected');
const viewScanning = document.getElementById('sim-view-scanning');
const viewPairing = document.getElementById('sim-view-pairing');
const viewConnected = document.getElementById('sim-view-connected');

// Action buttons
const btnConnect = document.getElementById('sim-btn-connect');
const btnBackToList = document.getElementById('sim-btn-back-to-list');
const btnVerify = document.getElementById('sim-btn-verify');
const btnDisconnect = document.getElementById('sim-btn-disconnect');
const btnSyncClipboard = document.getElementById('sim-btn-sync-clipboard');
const btnSendFile = document.getElementById('sim-btn-send-file');

const scanProgressBar = document.getElementById('sim-scan-progress');
const discoveredList = document.getElementById('sim-discovered-list');
const codeInputs = document.querySelectorAll('.sim-code-input') as NodeListOf<HTMLInputElement>;
const fileToast = document.getElementById('file-toast');

// Coord elements
const coordX = document.getElementById('mock-coord-x');
const coordY = document.getElementById('mock-coord-y');
const metricRtt = document.getElementById('metric-rtt');
const taskbarRttVal = document.getElementById('taskbar-rtt');

// Logs terminal triggers
const hostStatusIndicator = document.getElementById('host-status-indicator');
const hostStatusText = document.getElementById('host-status-text');
const simStatusIndicator = document.getElementById('sim-status-indicator');
const simStatusText = document.getElementById('sim-status-text');
const hostTerminalLogs = document.getElementById('host-terminal-logs');

// Graph & Stream canvas elements
const streamCanvas = document.getElementById('sim-stream-canvas') as HTMLCanvasElement | null;
const graphCanvas = document.getElementById('sim-graph-canvas') as HTMLCanvasElement | null;

let graphPoints: number[] = Array(20).fill(1.3);
let streamAnimFrameId: number;
let graphIntervalId: number;

let syncText = "Move cursor to sync.";
let isFileUploading = false;
let fileUploadProgress = 0;
let fileUploadName = "";

function logHost(msg: string, level: 'INFO' | 'SUCCESS' | 'WARN' = 'INFO') {
  if (!hostTerminalLogs) return;
  const now = new Date();
  const timeStr = `${now.getHours().toString().padStart(2, '0')}:${now.getMinutes().toString().padStart(2, '0')}:${now.getSeconds().toString().padStart(2, '0')}`;
  
  let colorClass = 'text-slate-500';
  if (level === 'SUCCESS') colorClass = 'text-emerald-400 font-semibold';
  if (level === 'WARN') colorClass = 'text-amber-400 font-semibold';

  const logDiv = document.createElement('div');
  logDiv.className = colorClass;
  logDiv.innerHTML = `[${timeStr}] ${level}: ${msg}`;
  
  hostTerminalLogs.appendChild(logDiv);
  hostTerminalLogs.scrollTop = hostTerminalLogs.scrollHeight;
}

function setSimStatus(state: 'offline' | 'scanning' | 'pairing' | 'connected') {
  simStatus = state;
  
  viewDisconnected?.classList.add('hidden');
  viewScanning?.classList.add('hidden');
  viewPairing?.classList.add('hidden');
  viewConnected?.classList.add('hidden');

  if (state !== 'connected') {
    if (streamAnimFrameId) cancelAnimationFrame(streamAnimFrameId);
    if (graphIntervalId) clearInterval(graphIntervalId);
  }

  // Update status UI labels
  if (simStatusIndicator && simStatusText) {
    if (state === 'offline') {
      simStatusIndicator.className = 'w-2 h-2 rounded-full bg-rose-500';
      simStatusText.innerText = 'Offline';
      simStatusText.className = 'text-[9px] uppercase tracking-wider text-rose-500 font-mono';
    } else if (state === 'scanning') {
      simStatusIndicator.className = 'w-2 h-2 rounded-full bg-cyan-500 animate-pulse';
      simStatusText.innerText = 'Scanning';
      simStatusText.className = 'text-[9px] uppercase tracking-wider text-cyan-500 font-mono';
    } else if (state === 'pairing') {
      simStatusIndicator.className = 'w-2 h-2 rounded-full bg-amber-500';
      simStatusText.innerText = 'Auth Handshake';
      simStatusText.className = 'text-[9px] uppercase tracking-wider text-amber-500 font-mono';
    } else if (state === 'connected') {
      simStatusIndicator.className = 'w-2 h-2 rounded-full bg-emerald-500 animate-pulse';
      simStatusText.innerText = 'Streaming';
      simStatusText.className = 'text-[9px] uppercase tracking-wider text-emerald-500 font-mono';
    }
  }

  if (hostStatusIndicator && hostStatusText) {
    if (state === 'connected') {
      hostStatusIndicator.className = 'w-2 h-2 rounded-full bg-emerald-500 animate-pulse';
      hostStatusText.innerText = 'Streaming';
      hostStatusText.className = 'text-[9px] uppercase tracking-wider text-emerald-500 font-bold';
    } else {
      hostStatusIndicator.className = 'w-2 h-2 rounded-full bg-amber-500';
      hostStatusText.innerText = 'Listening';
      hostStatusText.className = 'text-[9px] uppercase tracking-wider text-amber-500 font-bold';
    }
  }

  if (state === 'offline') {
    viewDisconnected?.classList.remove('hidden');
  } else if (state === 'scanning') {
    viewScanning?.classList.remove('hidden');
    startScanningAnimation();
  } else if (state === 'pairing') {
    viewPairing?.classList.remove('hidden');
    codeInputs.forEach(input => { input.value = ''; });
    enteredCode = ['', '', '', '', '', ''];
    codeInputs[0].focus();
  } else if (state === 'connected') {
    viewConnected?.classList.remove('hidden');
    startConnectedSimulators();
  }
}

function startScanningAnimation() {
  if (!scanProgressBar || !discoveredList) return;
  scanProgressBar.style.width = '0%';
  discoveredList.innerHTML = '<div class="text-slate-600 animate-pulse text-[9px] font-mono">Running socket survey...</div>';
  
  logHost('Initializing local subnet scanning client.');
  
  let progress = 0;
  const interval = setInterval(() => {
    progress += 10;
    scanProgressBar.style.width = `${progress}%`;
    
    if (progress === 30) {
      logHost('Probing range 192.168.1.1/24 on port 45101...');
    }
    
    if (progress === 60) {
      discoveredList.innerHTML = `
        <div class="p-2.5 rounded-xl bg-white/5 border border-white/5 hover:border-cyan-500/50 hover:bg-white/10 cursor-pointer transition flex items-center justify-between group" id="host-item-1">
          <div class="flex items-center space-x-2">
            <span class="relative flex h-1.5 w-1.5">
              <span class="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75"></span>
              <span class="relative inline-flex rounded-full h-1.5 w-1.5 bg-emerald-500"></span>
            </span>
            <div>
              <div class="text-white font-bold text-[10px]">DESKTOP-PRO-X</div>
              <div class="text-[8px] text-slate-500 font-mono">192.168.1.144 : 45101</div>
            </div>
          </div>
          <span class="text-[9px] text-cyan-400 font-mono group-hover:underline">Verify Key</span>
        </div>
      `;
      logHost('Identified host target listening at 192.168.1.144.');
      
      const hostItem = document.getElementById('host-item-1');
      hostItem?.addEventListener('click', () => {
        clearInterval(interval);
        setSimStatus('pairing');
      });
    }
    
    if (progress >= 100) {
      clearInterval(interval);
      logHost('LAN scan cycle completed.');
    }
  }, 120);
}

btnConnect?.addEventListener('click', () => setSimStatus('scanning'));
btnBackToList?.addEventListener('click', () => setSimStatus('scanning'));

codeInputs.forEach((input, index) => {
  input.addEventListener('input', (e) => {
    const val = (e.target as HTMLInputElement).value;
    if (!/^\d*$/.test(val)) {
      input.value = '';
      return;
    }
    enteredCode[index] = val;
    if (val !== '' && index < codeInputs.length - 1) {
      codeInputs[index + 1].focus();
    }
  });

  input.addEventListener('keydown', (e) => {
    if (e.key === 'Backspace' && input.value === '' && index > 0) {
      codeInputs[index - 1].focus();
    }
  });
});

function verifyPairingCode() {
  const code = enteredCode.join('');
  if (code === correctCode) {
    logHost('Establishing ECDHE TLS 1.3 socket pipe.');
    logHost('Handshake complete. Authorized session linked!', 'SUCCESS');
    logHost('Direct3D11 blit listener running.', 'SUCCESS');
    setSimStatus('connected');
  } else {
    logHost(`Authentication failure. Key match signature mismatch: ${code}`, 'WARN');
    codeInputs.forEach(input => {
      input.classList.add('border-red-500', 'bg-red-500/10');
      setTimeout(() => input.classList.remove('border-red-500', 'bg-red-500/10'), 800);
    });
  }
}

btnVerify?.addEventListener('click', verifyPairingCode);

// Mouse interaction positions
let mouseRelX = 0;
let mouseRelY = 0;

const streamContainer = document.getElementById('sim-stream-container');
const cursorPointer = document.getElementById('sim-cursor-pointer');

if (streamContainer && cursorPointer) {
  streamContainer.addEventListener('mousemove', (e) => {
    if (simStatus !== 'connected') return;
    const rect = streamContainer.getBoundingClientRect();
    mouseRelX = e.clientX - rect.left;
    mouseRelY = e.clientY - rect.top;
    
    cursorPointer.style.left = `${mouseRelX}px`;
    cursorPointer.style.top = `${mouseRelY}px`;
    cursorPointer.style.opacity = '1';
    
    if (coordX && coordY) {
      coordX.innerText = Math.round(mouseRelX).toString();
      coordY.innerText = Math.round(mouseRelY).toString();
    }
  });

  streamContainer.addEventListener('mouseleave', () => {
    if (simStatus !== 'connected') return;
    cursorPointer.style.opacity = '0';
  });
}

function startConnectedSimulators() {
  // A. Realtime HUD Chart in Taskbar
  const initGraph = () => {
    if (!graphCanvas) return;
    const ctx = graphCanvas.getContext('2d');
    
    const updateGraphData = () => {
      const lastVal = graphPoints[graphPoints.length - 1];
      let nextVal = lastVal + (Math.random() - 0.5) * 0.12;
      if (nextVal < 0.9) nextVal = 1.0;
      if (nextVal > 1.7) nextVal = 1.4;
      
      graphPoints.push(nextVal);
      graphPoints.shift();
      
      if (metricRtt) metricRtt.innerText = `${nextVal.toFixed(1)} ms`;
      if (taskbarRttVal) taskbarRttVal.innerText = `${nextVal.toFixed(1)} ms`;
      
      drawGraph();
    };

    const drawGraph = () => {
      if (!ctx) return;
      const width = graphCanvas.clientWidth;
      const height = graphCanvas.clientHeight;
      
      if (graphCanvas.width !== width || graphCanvas.height !== height) {
        graphCanvas.width = width;
        graphCanvas.height = height;
      }
      
      ctx.clearRect(0, 0, width, height);
      
      // Plot RTT line
      ctx.beginPath();
      const step = width / (graphPoints.length - 1);
      
      graphPoints.forEach((val, index) => {
        const valRatio = (val - 0.5) / 1.5;
        const x = index * step;
        const y = height - (valRatio * (height - 6) + 3);
        
        if (index === 0) {
          ctx.moveTo(x, y);
        } else {
          ctx.lineTo(x, y);
        }
      });
      
      ctx.strokeStyle = '#06b6d4';
      ctx.lineWidth = 1;
      ctx.stroke();
      
      ctx.lineTo(width, height);
      ctx.lineTo(0, height);
      ctx.closePath();
      ctx.fillStyle = 'rgba(6, 182, 212, 0.08)';
      ctx.fill();
    };

    graphIntervalId = window.setInterval(updateGraphData, 150);
    drawGraph();
  };

  // B. 60 FPS Remote Screen render
  const initStreamFeed = () => {
    if (!streamCanvas) return;
    const ctx = streamCanvas.getContext('2d');
    let frame = 0;
    
    let windowX = 40;
    let windowY = 30;
    let speedX = 0.4;
    let speedY = 0.2;
    
    const renderFeed = () => {
      if (!ctx) return;
      const w = streamCanvas.clientWidth;
      const h = streamCanvas.clientHeight;
      if (streamCanvas.width !== w || streamCanvas.height !== h) {
        streamCanvas.width = w;
        streamCanvas.height = h;
      }
      
      frame++;
      
      // Wallpaper Background
      ctx.fillStyle = '#060a16';
      ctx.fillRect(0, 0, w, h);
      
      // Desktop Grid overlay lines
      ctx.strokeStyle = 'rgba(255,255,255,0.015)';
      ctx.lineWidth = 0.5;
      const gap = 30;
      for (let x = 0; x < w; x += gap) {
        ctx.beginPath();
        ctx.moveTo(x, 0);
        ctx.lineTo(x, h);
        ctx.stroke();
      }
      for (let y = 0; y < h; y += gap) {
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(w, y);
        ctx.stroke();
      }

      // Draw Desktop file icons shortcuts
      const drawIcon = (x: number, y: number, label: string) => {
        ctx.fillStyle = 'rgba(6, 182, 212, 0.3)';
        ctx.fillRect(x, y, 12, 12);
        ctx.fillStyle = 'rgba(255,255,255,0.4)';
        ctx.font = '5px monospace';
        ctx.fillText(label, x - 2, y + 18);
      };
      drawIcon(20, 20, "cargo.toml");
      drawIcon(20, 50, "src_main");
      
      // Move a mock window frame
      windowX += speedX;
      windowY += speedY;
      if (windowX < 20 || windowX > w - 160) speedX *= -1;
      if (windowY < 15 || windowY > h - 100) speedY *= -1;
      
      // Render window
      ctx.fillStyle = '#020409';
      ctx.beginPath();
      ctx.roundRect(windowX, windowY, 120, 70, 6);
      ctx.fill();
      ctx.strokeStyle = 'rgba(255,255,255,0.06)';
      ctx.stroke();
      
      // Window header tab
      ctx.fillStyle = '#0d111c';
      ctx.beginPath();
      ctx.roundRect(windowX, windowY, 120, 14, [6, 6, 0, 0]);
      ctx.fill();
      
      ctx.fillStyle = 'rgba(255,255,255,0.5)';
      ctx.font = '5.5px monospace';
      ctx.fillText("Rust compile listener", windowX + 6, windowY + 9);
      
      // Code files inside window
      ctx.fillStyle = '#a855f7';
      ctx.fillText("fn render_loop() {", windowX + 8, windowY + 25);
      ctx.fillStyle = '#06b6d4';
      ctx.fillText("  let text = wgc::copy();", windowX + 8, windowY + 35);
      ctx.fillStyle = 'rgba(255,255,255,0.3)';
      ctx.fillText(`// frame: ${frame}`, windowX + 8, windowY + 45);
      ctx.fillText(`// CLIPBOARD: ${syncText.substring(0, 15)}`, windowX + 8, windowY + 55);
      
      // Draw upload metrics overlays
      if (isFileUploading) {
        ctx.fillStyle = 'rgba(2, 4, 10, 0.95)';
        ctx.fillRect(w/2 - 50, h/2 - 20, 100, 35);
        ctx.strokeStyle = '#06b6d4';
        ctx.strokeRect(w/2 - 50, h/2 - 20, 100, 35);
        
        ctx.fillStyle = '#fff';
        ctx.font = '5px monospace';
        ctx.fillText(`Write: ${fileUploadName}`, w/2 - 42, h/2 - 10);
        
        ctx.fillStyle = 'rgba(255,255,255,0.1)';
        ctx.fillRect(w/2 - 42, h/2 - 4, 84, 3);
        ctx.fillStyle = '#06b6d4';
        ctx.fillRect(w/2 - 42, h/2 - 4, fileUploadProgress * 0.84, 3);
        
        ctx.fillStyle = '#06b6d4';
        ctx.fillText(`Progress: ${Math.round(fileUploadProgress)}%`, w/2 - 42, h/2 + 8);
      }

      // Windows OS Dock Taskbar
      ctx.fillStyle = '#03050a';
      ctx.fillRect(0, h - 14, w, 14);
      ctx.fillStyle = 'rgba(255,255,255,0.3)';
      ctx.fillRect(6, h - 11, 8, 8);
      ctx.fillRect(18, h - 11, 8, 8);

      streamAnimFrameId = requestAnimationFrame(renderFeed);
    };
    renderFeed();
  };

  initGraph();
  initStreamFeed();
}

btnSyncClipboard?.addEventListener('click', () => {
  const text = prompt('Type text to synchronize with remote host clipboard:', 'Vite Tailwind integration is smooth');
  if (text && text.trim() !== '') {
    syncText = text;
    logHost(`Received ControlMessage::ClipboardSync { text: "${text}" }`);
    logHost(`Host clipboard updated. Sync buffers updated.`, 'SUCCESS');
  }
});

btnSendFile?.addEventListener('click', () => {
  if (isFileUploading) return;
  fileUploadName = "setup_archive.zip";
  logHost('Initializing block file transfer: setup_archive.zip (412 KB)');
  logHost('Chunk packet headers generated. Streaming blocks 1..8...');
  
  isFileUploading = true;
  fileUploadProgress = 0;
  
  const uploadInterval = setInterval(() => {
    fileUploadProgress += 10;
    if (fileUploadProgress >= 100) {
      fileUploadProgress = 100;
      clearInterval(uploadInterval);
      
      logHost('All file blocks received on Host socket.', 'SUCCESS');
      logHost('SHA-256 integrity verification code: PASSED.', 'SUCCESS');
      logHost('File setup_archive.zip written to Host Downloads.', 'SUCCESS');
      
      if (fileToast) {
        fileToast.classList.remove('opacity-0', 'translate-y-8');
        fileToast.classList.add('opacity-100', 'translate-y-0');
        
        setTimeout(() => {
          fileToast.classList.add('opacity-0', 'translate-y-8');
          fileToast.classList.remove('opacity-100', 'translate-y-0');
          isFileUploading = false;
        }, 1500);
      }
    }
  }, 100);
});

// Drag/drop zones on canvas stream container
const dragDropZone = document.getElementById('sim-drag-drop-zone');
if (streamContainer && dragDropZone) {
  streamContainer.addEventListener('dragover', (e) => {
    e.preventDefault();
    dragDropZone.classList.remove('opacity-0');
  });

  dragDropZone.addEventListener('dragleave', () => {
    dragDropZone.classList.add('opacity-0');
  });

  dragDropZone.addEventListener('drop', (e) => {
    e.preventDefault();
    dragDropZone.classList.add('opacity-0');
    btnSendFile?.click();
  });
}

btnDisconnect?.addEventListener('click', () => {
  logHost('Disconnect signal received. Stopping encoder stream.');
  logHost('Closed capture descriptors. Freeing thread blocks.');
  logHost('Connection closed cleanly.', 'WARN');
  setSimStatus('offline');
});


// ==========================================
// 5. ARCHITECTURE DIAGRAM DETAIL SWITCHER
// ==========================================
const archNodes = document.querySelectorAll('.arch-node');
const archDetailTitle = document.getElementById('arch-detail-title');
const archDetailDesc = document.getElementById('arch-detail-desc');

const nodeDetails: Record<string, { title: string; desc: string }> = {
  wgc: {
    title: '📡 Windows Graphics Capture (WGC)',
    desc: 'Bypasses legacy GDI screen scraping. Beacon binds directly into the Windows Compositor API using COM. Direct3D11 device cache is used to share captured frames inside VRAM, avoiding copy copies into system RAM.'
  },
  encoder: {
    title: '🚀 Media Foundation Hardware Encoder',
    desc: 'Encodes D3D11 VRAM textures to H.264 bitstream using physical GPU cores (NVENC, AMF, or QSV). Features custom low-latency profiles, zero frame-buffering, and constant rate target ceilings.'
  },
  udp: {
    title: '⚡ RTP Stream over UDP (Port :45100)',
    desc: 'Streams slice blocks over UDP. Features custom Forward Error Correction (FEC) packet rebuilding. Integrates a smart ring-buffer that drops older frames on network jitter spikes to prevent visual lags.'
  },
  tcp: {
    title: '🔒 Secure TLS 1.3 Control Channel (Port :45101)',
    desc: 'Transfers cursor coordinates, keyboard scancodes, clipboard buffers, and file segments. Binds over rustls under strict TLS 1.3 encryption, using self-signed keys rotated dynamically on launch.'
  },
  decoder: {
    title: '🖥️ H.264 Client Decoder',
    desc: 'Translates incoming RTP byte packets to direct direct display buffers inside the Pulse client player app. Interacts with Windows Media Foundation IMFTransform decoders for hardware speed.'
  },
  renderer: {
    title: '🎨 Direct Blit Win32 Rendering',
    desc: 'Renders GPU texture surfaces directly to Win32 window displays. Employs coordinate scaling filters to subtract border offsets and letterbox gaps, mapping clicks back to correct host layouts.'
  }
};

archNodes.forEach(node => {
  node.addEventListener('click', () => {
    const key = node.getAttribute('data-node') || '';
    const detail = nodeDetails[key];
    
    if (detail && archDetailTitle && archDetailDesc) {
      archDetailTitle.innerHTML = `
        <span class="w-1.5 h-1.5 rounded-full bg-cyan-400 animate-pulse"></span>
        <span>${detail.title}</span>
      `;
      archDetailDesc.innerText = detail.desc;
      
      archNodes.forEach(n => {
        const rect = n.querySelector('rect');
        if (rect) rect.setAttribute('stroke', 'rgba(255,255,255,0.08)');
      });
      const activeRect = node.querySelector('rect');
      if (activeRect) activeRect.setAttribute('stroke', '#06b6d4');
    }
  });
});


// ==========================================
// 6. DEVELOPER CLI CONFIG LAUNCHER
// ==========================================
const cliTabHost = document.getElementById('cli-tab-host');
const cliTabPlayer = document.getElementById('cli-tab-player');
const cliOptTarget = document.getElementById('cli-opt-target');
const cliInputTarget = document.getElementById('cli-input-target') as HTMLSelectElement;
const cliInputBitrate = document.getElementById('cli-input-bitrate') as HTMLInputElement;
const cliLblBitrate = document.getElementById('cli-lbl-bitrate');
const cliBtnFps = document.querySelectorAll('.cli-btn-fps');
const cliToggleClipboard = document.getElementById('cli-toggle-clipboard');
const cliCmdDisplay = document.getElementById('cli-cmd-display');
const cliCmdDesc = document.getElementById('cli-cmd-desc');
const cliBtnCopy = document.getElementById('cli-btn-copy');

let cliMode: 'host' | 'player' = 'host';
let targetFlag = '--window "Chrome"';
let bitrateVal = 20;
let fpsVal = 60;
let clipboardVal = true;

cliTabHost?.addEventListener('click', () => {
  cliMode = 'host';
  cliTabHost.className = 'flex-grow py-1.5 text-[9px] font-bold rounded-md bg-purple-600 text-white transition-all';
  cliTabPlayer!.className = 'flex-grow py-1.5 text-[9px] font-bold rounded-md text-slate-400 hover:text-white transition-all';
  cliOptTarget?.classList.remove('hidden');
  updateCliCommand();
});

cliTabPlayer?.addEventListener('click', () => {
  cliMode = 'player';
  cliTabPlayer.className = 'flex-grow py-1.5 text-[9px] font-bold rounded-md bg-cyan-600 text-white transition-all';
  cliTabHost!.className = 'flex-grow py-1.5 text-[9px] font-bold rounded-md text-slate-400 hover:text-white transition-all';
  cliOptTarget?.classList.add('hidden');
  updateCliCommand();
});

cliInputTarget?.addEventListener('change', (e) => {
  targetFlag = (e.target as HTMLSelectElement).value;
  updateCliCommand();
});

cliInputBitrate?.addEventListener('input', (e) => {
  bitrateVal = parseInt((e.target as HTMLInputElement).value);
  if (cliLblBitrate) cliLblBitrate.innerText = bitrateVal.toString();
  updateCliCommand();
});

cliBtnFps.forEach(btn => {
  btn.addEventListener('click', () => {
    cliBtnFps.forEach(b => {
      b.classList.remove('border-purple-500', 'border-cyan-500', 'bg-purple-950/20', 'bg-cyan-950/20', 'text-purple-400', 'text-cyan-400');
      b.classList.add('border-white/5', 'bg-[#03060f]', 'text-slate-400');
    });
    
    fpsVal = parseInt(btn.getAttribute('data-value') || '60');
    const activeColor = cliMode === 'host' ? 'border-purple-500 bg-purple-950/20 text-purple-400' : 'border-cyan-500 bg-cyan-950/20 text-cyan-400';
    btn.className = `cli-btn-fps py-1.5 text-[9px] rounded-lg border font-bold ${activeColor}`;
    
    updateCliCommand();
  });
});

cliToggleClipboard?.addEventListener('click', () => {
  clipboardVal = !clipboardVal;
  const toggleSpan = cliToggleClipboard.querySelector('span');
  
  if (clipboardVal) {
    cliToggleClipboard.className = 'w-8 h-4.5 rounded-full bg-purple-600 p-0.5 transition-all focus:outline-none flex items-center justify-end';
    toggleSpan?.classList.remove('-translate-x-3.5');
  } else {
    cliToggleClipboard.className = 'w-8 h-4.5 rounded-full bg-slate-700 p-0.5 transition-all focus:outline-none flex items-center justify-start';
    toggleSpan?.classList.add('translate-x-0');
  }
  
  updateCliCommand();
});

function updateCliCommand() {
  if (!cliCmdDisplay || !cliCmdDesc) return;
  
  if (cliMode === 'host') {
    const hostCommand = `.\\beacon.exe host ${targetFlag} --quality ${bitrateVal} --fps ${fpsVal} --clipboard ${clipboardVal}`;
    cliCmdDisplay.innerText = hostCommand;
    
    let desc = `Launches the Beacon Host sharing the selected window/display. Enforces a stream ceiling of ${bitrateVal} Mbps at ${fpsVal} FPS. `;
    desc += clipboardVal ? 'Permits local-remote clipboard sync.' : 'Disables background clipboard reading.';
    cliCmdDesc.innerText = desc;
  } else {
    const playerCommand = `.\\pulse.exe play --host 192.168.1.144 --code 582914`;
    cliCmdDisplay.innerText = playerCommand;
    cliCmdDesc.innerText = 'Launches the Pulse Client Player. Skips local host search scanning, connecting directly to the host node IP 192.168.1.144 with pairing code validation.';
  }
}

cliBtnCopy?.addEventListener('click', () => {
  if (cliCmdDisplay) {
    navigator.clipboard.writeText(cliCmdDisplay.innerText).then(() => {
      const copySpan = cliBtnCopy.querySelector('span');
      if (copySpan) {
        copySpan.innerText = 'Copied!';
        setTimeout(() => {
          copySpan.innerText = 'Copy';
        }, 1500);
      }
    });
  }
});
