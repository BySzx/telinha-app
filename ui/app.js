import { initReactions, showReaction } from './reactions.js';
import { createSfu } from './sfu.js';
import { isNative, listAudioApps, startNativeAudio, bindWindowControls, appVersion, checkUpdate, installUpdate } from './native.js';

const SERVER = window.TELINHA_SERVER ?? 'https://telinha-xp0y.onrender.com';
const WS_URL = SERVER.replace(/^http/, 'ws');
const ROOM_RE = /^[A-Za-z0-9_-]{4,64}$/;

const QUALITIES = {
  '720p30': { label: '720p 30', height: 720, fps: 30, bitrate: 2_500_000 },
  '720p60': { label: '720p 60', height: 720, fps: 60, bitrate: 4_000_000 },
  '1080p30': { label: '1080p 30', height: 1080, fps: 30, bitrate: 4_500_000 },
  '1080p60': { label: '1080p 60', height: 1080, fps: 60, bitrate: 7_000_000 },
  '1440p60': { label: '1440p 60', height: 1440, fps: 60, bitrate: 12_000_000 },
  '2160p30': { label: '4K 30', height: 2160, fps: 30, bitrate: 16_000_000 },
  '2160p60': { label: '4K 60', height: 2160, fps: 60, bitrate: 25_000_000 },
};

const $ = (sel) => document.querySelector(sel);

const storage = {
  get(key) {
    try { return localStorage.getItem(key); } catch { return null; }
  },
  set(key, value) {
    try { localStorage.setItem(key, value); } catch {}
  },
};

const randomId = () => {
  const bytes = crypto.getRandomValues(new Uint8Array(8));
  return Array.from(bytes, (b) => b.toString(36).padStart(2, '0')).join('').slice(0, 12);
};

const peers = new Map();
let ws = null;
let iceServers = [];
let mode = 'p2p';
let sfu = null;
let myId = null;
let myName = '';
let roomId = '';
let screenStream = null;
let displayStream = null;
let nativeAudio = null;
let localTile = null;
let focusedTile = null;
let qualityKey = (storage.get('quality') ?? '') in QUALITIES ? storage.get('quality') : '1080p30';
let audioChoice = { kind: isNative ? 'system' : 'browser' };
let audioApps = [];
let toastTimer = 0;

bindWindowControls();
setupUpdates();
$('#name').value = storage.get('name') ?? '';

function parseRoom(input) {
  const value = input.trim();
  if (!value) return null;
  try {
    const r = new URL(value).searchParams.get('r');
    if (r && ROOM_RE.test(r)) return r;
  } catch {}
  return ROOM_RE.test(value) ? value : null;
}

function lobbyError(message) {
  const el = $('#lobbyError');
  el.textContent = message;
  el.classList.remove('shake');
  void el.offsetWidth;
  el.classList.add('shake');
}

function toast(message, ms = 4500) {
  const el = $('#toast');
  el.textContent = message;
  el.hidden = false;
  el.style.animation = 'none';
  void el.offsetWidth;
  el.style.animation = '';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), ms);
}

async function enter(targetRoom) {
  const name = $('#name').value.trim();
  if (!name) return lobbyError('Coloca seu nome primeiro.');
  if (!targetRoom) return lobbyError('Código ou link de sala inválido.');
  myName = name;
  roomId = targetRoom;
  storage.set('name', name);
  document.querySelectorAll('#lobbyForm .btn').forEach((b) => (b.disabled = true));
  $('#lobbyError').textContent = '';
  try {
    await join();
  } catch (err) {
    lobbyError(err instanceof Error ? err.message : String(err));
    document.querySelectorAll('#lobbyForm .btn').forEach((b) => (b.disabled = false));
  }
}

$('#createRoom').addEventListener('click', () => enter(randomId()));
$('#lobbyForm').addEventListener('submit', (ev) => {
  ev.preventDefault();
  enter(parseRoom($('#roomInput').value));
});

async function join() {
  const config = await fetch(`${SERVER}/config`)
    .then((r) => r.json())
    .catch(() => {
      throw new Error('Servidor fora do ar. Tenta de novo em alguns segundos (ele pode estar acordando).');
    });
  iceServers = config.iceServers;

  ws = new WebSocket(WS_URL);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', () => reject(new Error('Não foi possível conectar ao servidor.')), { once: true });
  });

  ws.addEventListener('message', (ev) => onServerMessage(JSON.parse(ev.data)));
  ws.addEventListener('close', () => {
    if (!myId) return;
    for (const id of [...peers.keys()]) removePeer(id);
    toast('Desconectado do servidor. Saia e entre de novo.', 8000);
  });

  send({ t: 'join', room: roomId, name: myName, state: myState(), sfu: typeof RTCRtpSender !== 'undefined' });
}

function send(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function signal(to, data) {
  send({ t: 'signal', to, data });
}

function myState() {
  return { screen: screenStream?.id ?? null };
}

function onServerMessage(msg) {
  switch (msg.t) {
    case 'welcome':
      myId = msg.id;
      mode = msg.mode === 'sfu' ? 'sfu' : 'p2p';
      if (mode === 'sfu') startSfu();
      enterRoom();
      for (const info of msg.peers) createPeer(info);
      updateCounters();
      break;
    case 'joined':
      createPeer(msg.peer);
      updateCounters();
      toast(`${msg.peer.name} entrou na sala`, 2500);
      break;
    case 'left': {
      const name = peers.get(msg.id)?.name;
      removePeer(msg.id);
      updateCounters();
      if (name) toast(`${name} saiu`, 2500);
      break;
    }
    case 'state': {
      const peer = peers.get(msg.id);
      if (!peer) break;
      const prevScreen = peer.state.screen;
      peer.state = msg.state;
      if (mode === 'p2p' && prevScreen && prevScreen !== msg.state.screen) removeRemoteTile(peer);
      renderPeople();
      break;
    }
    case 'signal': {
      const peer = peers.get(msg.from);
      if (!peer?.pc) break;
      peer.queue = peer.queue.then(() => handleSignal(peer, msg.data)).catch((err) => console.warn(err));
      break;
    }
    case 'sfu':
      sfu?.handle(msg);
      break;
    case 'mode':
      if (msg.mode === 'p2p' && mode === 'sfu') switchToP2p();
      break;
    case 'react':
      showReaction(msg.name, msg.r);
      break;
    case 'error':
      lobbyError(msg.reason === 'full' ? 'Sala cheia (máx. 10).' : 'Dados inválidos.');
      document.querySelectorAll('#lobbyForm .btn').forEach((b) => (b.disabled = false));
      break;
  }
}

function enterRoom() {
  $('#lobby').hidden = true;
  $('#room').hidden = false;
  $('#roomLabel').textContent = roomId;
  document.body.classList.add('in-room');
  startTintLoop();
  initReactions((r) => send({ t: 'react', r }));
}

function createPeer(info) {
  const peer = {
    id: info.id,
    name: info.name,
    state: info.state,
    pc: null,
    polite: myId > info.id,
    makingOffer: false,
    ignoreOffer: false,
    queue: Promise.resolve(),
    screenTx: [],
    tile: null,
  };
  peers.set(peer.id, peer);
  if (mode === 'p2p') attachPc(peer);
  else if (sfuStreams.has(peer.id)) showRemoteTile(peer, sfuStreams.get(peer.id));
  return peer;
}

const sfuStreams = new Map();

function startSfu() {
  sfu = createSfu({
    send,
    iceServers,
    onStream(owner, stream) {
      sfuStreams.set(owner, stream);
      const peer = peers.get(owner);
      if (peer) showRemoteTile(peer, stream);
    },
    onStreamGone(owner) {
      sfuStreams.delete(owner);
      const peer = peers.get(owner);
      if (peer) removeRemoteTile(peer);
    },
    onState() {
      for (const peer of peers.values()) updateTileStatus(peer);
    },
  });
}

function switchToP2p() {
  mode = 'p2p';
  sfu?.close();
  sfu = null;
  sfuStreams.clear();
  for (const peer of peers.values()) {
    removeRemoteTile(peer);
    attachPc(peer);
  }
}

function attachPc(peer) {
  const pc = new RTCPeerConnection({ iceServers });
  peer.pc = pc;

  pc.onnegotiationneeded = async () => {
    try {
      peer.makingOffer = true;
      await pc.setLocalDescription();
      signal(peer.id, { description: pc.localDescription });
    } catch (err) {
      console.warn(err);
    } finally {
      peer.makingOffer = false;
    }
  };

  pc.onicecandidate = ({ candidate }) => {
    if (candidate) signal(peer.id, { candidate });
  };

  pc.ontrack = ({ track, streams }) => {
    if (track.kind === 'video') showRemoteTile(peer, streams[0] ?? new MediaStream([track]));
  };

  pc.onconnectionstatechange = () => {
    if (pc.connectionState === 'failed') pc.restartIce();
    updateTileStatus(peer);
  };

  if (screenStream) addScreenTo(peer);
}

async function handleSignal(peer, { description, candidate }) {
  const { pc } = peer;
  if (description) {
    const collision = description.type === 'offer' && (peer.makingOffer || pc.signalingState !== 'stable');
    peer.ignoreOffer = !peer.polite && collision;
    if (peer.ignoreOffer) return;
    await pc.setRemoteDescription(description);
    if (description.type === 'offer') {
      await pc.setLocalDescription();
      signal(peer.id, { description: pc.localDescription });
    }
  } else if (candidate) {
    try {
      await pc.addIceCandidate(candidate);
    } catch (err) {
      if (!peer.ignoreOffer) throw err;
    }
  }
}

function removePeer(id) {
  const peer = peers.get(id);
  if (!peer) return;
  peer.pc?.close();
  removeRemoteTile(peer);
  peers.delete(id);
}

function showRemoteTile(peer, stream) {
  if (peer.tile?.stream === stream) return;
  removeRemoteTile(peer);
  peer.tile = makeTile(peer.name, stream, false);
  updateTileStatus(peer);
  setTimeout(() => {
    if (peer.tile?.stream === stream && (peer.pc?.connectionState ?? sfu?.state) !== 'connected') {
      peer.tile.status.textContent = 'Sem conexão. Desligue VPN/proxy e tente de novo.';
    }
  }, 15_000);
  stream.addEventListener('removetrack', () => {
    if (peer.tile?.stream === stream && stream.getVideoTracks().length === 0) removeRemoteTile(peer);
  });
}

function updateTileStatus(peer) {
  if (!peer.tile) return;
  const state = peer.pc ? peer.pc.connectionState : sfu?.state ?? 'new';
  const { status, statusText } = peer.tile;
  status.hidden = state === 'connected';
  status.classList.toggle('failed', state === 'failed');
  if (state === 'failed') statusText.textContent = 'Falha na conexão. Desligue VPN/proxy e tente de novo.';
  else if (state !== 'connected') statusText.textContent = 'Conectando…';
}

function removeRemoteTile(peer) {
  if (!peer.tile) return;
  destroyTile(peer.tile);
  peer.tile = null;
}

function makeTile(label, stream, isLocal) {
  const el = document.createElement('div');
  el.className = 'tile';

  const video = document.createElement('video');
  video.autoplay = true;
  video.playsInline = true;
  video.muted = isLocal;
  video.srcObject = stream;

  const status = document.createElement('div');
  status.className = 'tile-status';
  status.hidden = true;
  status.append(eyes({ w: 10, h: 16, gap: 10, mood: 'busy' }));
  const statusText = document.createElement('span');
  status.append(statusText);

  const bar = document.createElement('div');
  bar.className = 'tile-bar';

  const name = document.createElement('span');
  name.className = 'name';
  name.textContent = isLocal ? `${label} (você)` : label;
  bar.append(name);

  if (!isLocal) {
    const wrap = document.createElement('label');
    wrap.className = 'vol';
    wrap.title = 'Volume';
    wrap.innerHTML = ICONS.volume;
    const vol = document.createElement('input');
    vol.type = 'range';
    vol.min = '0';
    vol.max = '1';
    vol.step = '0.05';
    vol.value = '1';
    vol.addEventListener('input', () => {
      video.volume = Number(vol.value);
      vol.style.setProperty('--v', vol.value);
    });
    wrap.append(vol);
    bar.append(wrap);
  }

  const fs = document.createElement('button');
  fs.type = 'button';
  fs.className = 'icon-btn';
  fs.innerHTML = ICONS.fullscreen;
  fs.title = 'Tela cheia';
  bar.append(fs);

  el.append(video, status, bar);
  $('#stage').append(el);

  const tile = { el, video, stream, status, statusText };
  video.addEventListener('click', () => toggleFocus(tile));
  video.addEventListener('dblclick', () => fullscreen(el));
  fs.addEventListener('click', () => fullscreen(el));
  video.play().catch(() => {});
  updateStage();
  return tile;
}

function destroyTile(tile) {
  if (focusedTile === tile) focusedTile = null;
  tile.video.srcObject = null;
  tile.el.remove();
  updateStage();
}

function toggleFocus(tile) {
  focusedTile?.el.classList.remove('focused');
  focusedTile = focusedTile === tile ? null : tile;
  focusedTile?.el.classList.add('focused');
  updateStage();
}

function fullscreen(el) {
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  else el.requestFullscreen().catch(() => {});
}

function updateStage() {
  const stage = $('#stage');
  stage.classList.toggle('has-focus', !!focusedTile);
  $('#emptyStage').hidden = stage.querySelector('.tile') !== null;
}

function renderQualityChips() {
  const box = $('#qualityChips');
  box.replaceChildren(
    ...Object.entries(QUALITIES).map(([key, q]) => {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.className = `chip${key === qualityKey ? ' selected' : ''}`;
      chip.textContent = q.label;
      chip.addEventListener('click', () => {
        qualityKey = key;
        storage.set('quality', key);
        renderQualityChips();
        applyQuality();
      });
      return chip;
    }),
  );
  updateUploadHint();
}

function updateUploadHint() {
  const viewers = peers.size;
  const mbps = (QUALITIES[qualityKey].bitrate * viewers) / 1_000_000;
  const hint = $('#uploadHint');
  hint.textContent = viewers
    ? `Upload necessário: ~${mbps.toFixed(1)} Mbps pra ${viewers} ${viewers === 1 ? 'pessoa' : 'pessoas'}`
    : 'Ninguém na sala ainda. O upload depende de quantas pessoas assistem.';
  hint.classList.toggle('warn', mbps > 40);
}

function audioOption({ key, title, subtitle, mono = false, badge, choice }) {
  const label = document.createElement('label');
  const selected = JSON.stringify(choice) === JSON.stringify(audioChoice);
  label.className = `option${selected ? ' selected' : ''}`;
  label.dataset.key = key;

  const input = document.createElement('input');
  input.type = 'radio';
  input.name = 'audio';
  input.checked = selected;

  const radio = document.createElement('span');
  radio.className = 'radio';

  const text = document.createElement('span');
  text.className = 'label';
  const b = document.createElement('b');
  b.textContent = title;
  text.append(b);
  if (subtitle) {
    const small = document.createElement('small');
    if (mono) small.className = 'mono';
    small.textContent = subtitle;
    text.append(small);
  }

  label.append(input, radio, text);
  if (badge) {
    const tag = document.createElement('span');
    tag.className = 'badge';
    tag.textContent = badge;
    label.append(tag);
  }
  label.addEventListener('click', (ev) => {
    ev.preventDefault();
    audioChoice = choice;
    renderAudioOptions();
  });
  return label;
}

function renderAudioOptions() {
  const box = $('#audioOptions');
  const items = [];
  if (isNative) {
    items.push(audioOption({ key: 'system', title: 'Todo o som do PC', subtitle: 'Menos o próprio Telinha, então não dá eco', choice: { kind: 'system' } }));
    if (audioApps.length) {
      const group = document.createElement('div');
      group.className = 'options-group';
      group.textContent = 'Só o som de um app';
      items.push(group);
      for (const app of audioApps) {
        items.push(audioOption({
          key: `app-${app.pid}`,
          title: app.name,
          subtitle: `PID ${app.pid}`,
          mono: true,
          badge: app.active ? 'TOCANDO' : '',
          choice: { kind: 'app', pid: app.pid },
        }));
      }
    } else {
      const group = document.createElement('div');
      group.className = 'options-group';
      group.textContent = 'Nenhum app tocando som agora. Abra o jogo ou vídeo e clique em Atualizar apps.';
      items.push(group);
    }
  } else {
    items.push(audioOption({ key: 'browser', title: 'Som do navegador', subtitle: 'Marque “Compartilhar áudio” na janela de escolha', choice: { kind: 'browser' } }));
  }
  items.push(audioOption({ key: 'none', title: 'Sem áudio', subtitle: 'Só a imagem', choice: { kind: 'none' } }));
  box.replaceChildren(...items);
}

async function refreshAudioApps() {
  if (!isNative) return;
  try {
    audioApps = await listAudioApps();
  } catch (err) {
    audioApps = [];
    toast(`Não deu pra listar os apps: ${err}`);
  }
  if (audioChoice.kind === 'app' && !audioApps.some((a) => a.pid === audioChoice.pid)) audioChoice = { kind: 'system' };
  renderAudioOptions();
}

function openSheet() {
  renderQualityChips();
  renderAudioOptions();
  $('#refreshApps').hidden = !isNative;
  const backdrop = $('#sheetBackdrop');
  backdrop.classList.remove('closing');
  backdrop.hidden = false;
  refreshAudioApps();
}

function closeSheet() {
  const backdrop = $('#sheetBackdrop');
  if (backdrop.hidden) return;
  backdrop.classList.add('closing');
  setTimeout(() => {
    backdrop.hidden = true;
    backdrop.classList.remove('closing');
  }, 200);
}

$('#refreshApps').addEventListener('click', refreshAudioApps);
$('#cancelShare').addEventListener('click', closeSheet);
$('#sheetBackdrop').addEventListener('click', (ev) => {
  if (ev.target === ev.currentTarget) closeSheet();
});
$('#shareSheet').addEventListener('submit', (ev) => {
  ev.preventDefault();
  closeSheet();
  startShare();
});

async function startShare() {
  const q = QUALITIES[qualityKey];
  const wantsBrowserAudio = !isNative && audioChoice.kind === 'browser';
  let display;
  try {
    display = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: { ideal: q.fps, max: q.fps }, height: { ideal: q.height, max: q.height } },
      audio: wantsBrowserAudio ? { echoCancellation: false, noiseSuppression: false, autoGainControl: false } : false,
      systemAudio: 'include',
      surfaceSwitching: 'include',
    });
  } catch {
    return;
  }

  const [video] = display.getVideoTracks();
  video.contentHint = 'motion';
  video.addEventListener('ended', stopShare, { once: true });

  let audioTracks = display.getAudioTracks();
  if (isNative && audioChoice.kind !== 'none') {
    try {
      nativeAudio = await startNativeAudio(audioChoice.kind === 'app' ? { kind: 'app', pid: audioChoice.pid } : { kind: 'system' });
      audioTracks = [nativeAudio.track];
    } catch (err) {
      nativeAudio = null;
      toast(`Áudio indisponível (${err}). Transmitindo só a imagem.`, 7000);
    }
  } else if (wantsBrowserAudio && !audioTracks.length) {
    toast('O navegador não mandou áudio. Escolha "Tela inteira" e marque "Compartilhar áudio".', 7000);
  }

  displayStream = display;
  screenStream = new MediaStream([video, ...audioTracks]);
  send({ t: 'state', state: myState() });
  await hardwareCodecPrefs();
  if (sfu) await sfu.publish(screenStream, QUALITIES[qualityKey], codecPrefs);
  for (const peer of peers.values()) addScreenTo(peer);

  localTile = makeTile(myName, screenStream, true);
  startShareStats(localTile);
  $('#shareLabel').textContent = 'Parar';
  $('#share').classList.add('active');
  updateCounters();
}

function addScreenTo(peer) {
  if (!screenStream || !peer.pc) return;
  const q = QUALITIES[qualityKey];
  peer.screenTx = screenStream.getTracks().map((track) => {
    const tx = peer.pc.addTransceiver(track, {
      direction: 'sendonly',
      streams: [screenStream],
      ...(track.kind === 'video' && { sendEncodings: [{ maxBitrate: q.bitrate, maxFramerate: q.fps }] }),
    });
    if (track.kind === 'video' && codecPrefs?.length) {
      try { tx.setCodecPreferences(codecPrefs); } catch {}
    }
    return tx;
  });
}

let codecEfficient = new Set();
let statsTimer = 0;
let lastBytes = 0;
let lastStatsAt = 0;

const CODEC_NAMES = { 'video/H264': 'H.264', 'video/AV1': 'AV1', 'video/VP9': 'VP9', 'video/VP8': 'VP8' };
const LIMITS = { cpu: 'processador no limite', bandwidth: 'internet no limite' };

function videoSenders() {
  const list = [...peers.values()].flatMap((p) => p.screenTx.map((tx) => tx.sender));
  return [...list, ...(sfu?.videoSenders() ?? [])].filter((s) => s.track?.kind === 'video');
}

function startShareStats(tile) {
  const box = document.createElement('div');
  box.className = 'share-stats';
  tile.el.append(box);
  lastBytes = 0;
  lastStatsAt = 0;
  clearInterval(statsTimer);
  const tick = async () => {
    if (!screenStream || !box.isConnected) return clearInterval(statsTimer);
    const reports = await Promise.all(videoSenders().map((s) => s.getStats().catch(() => null)));
    let codec = '';
    let hw = null;
    let fps = Infinity;
    let height = 0;
    let bytes = 0;
    let limit = '';
    let streams = 0;
    for (const report of reports) {
      report?.forEach((st) => {
        if (st.type !== 'outbound-rtp' || st.kind !== 'video') return;
        streams++;
        bytes += st.bytesSent ?? 0;
        if (st.framesPerSecond != null) fps = Math.min(fps, st.framesPerSecond);
        height = Math.max(height, st.frameHeight ?? 0);
        if (st.qualityLimitationReason && st.qualityLimitationReason !== 'none') limit = st.qualityLimitationReason;
        const c = report.get(st.codecId);
        if (c) codec = c.mimeType;
        if (st.powerEfficientEncoder != null) hw = st.powerEfficientEncoder;
        else if (/MediaFoundation|Nvenc|Amf|Qsv|VideoToolbox|VAAPI|V4L2/i.test(st.encoderImplementation ?? '')) hw = true;
      });
    }
    const now = performance.now();
    const mbps = lastStatsAt ? ((bytes - lastBytes) * 8) / ((now - lastStatsAt) * 1000) : 0;
    lastBytes = bytes;
    lastStatsAt = now;
    if (hw === null && codec) hw = codecEfficient.has(codec);
    const viewers = peers.size;
    const chips = [];
    if (!streams) {
      chips.push(['', viewers ? 'Conectando…' : 'Ninguém assistindo ainda']);
    } else {
      chips.push(['', CODEC_NAMES[codec] ?? codec.replace('video/', '')]);
      chips.push([hw ? 'ok' : 'warn', hw ? 'placa de vídeo' : 'processador']);
      if (Number.isFinite(fps)) chips.push([fps < (QUALITIES[qualityKey].fps * 0.75) ? 'warn' : '', `${Math.round(fps)} fps`]);
      if (height) chips.push(['', `${height}p`]);
      if (mbps > 0) chips.push(['', `${mbps.toFixed(1)} Mbps`]);
      chips.push(['', `${viewers} assistindo`]);
      if (limit) chips.push(['warn', LIMITS[limit] ?? limit]);
    }
    box.replaceChildren(
      ...chips.map(([cls, text]) => {
        const span = document.createElement('span');
        if (cls) span.className = cls;
        span.textContent = text;
        return span;
      }),
    );
  };
  statsTimer = setInterval(tick, 1000);
  tick();
}

let codecPrefs = null;

async function hardwareCodecPrefs() {
  if (codecPrefs) return codecPrefs;
  const caps = globalThis.RTCRtpSender?.getCapabilities?.('video')?.codecs ?? [];
  const efficient = new Set();
  for (const mime of ['video/H264', 'video/AV1', 'video/VP9']) {
    try {
      const info = await navigator.mediaCapabilities.encodingInfo({
        type: 'webrtc',
        video: { contentType: mime, width: 1920, height: 1080, bitrate: 8_000_000, framerate: 60 },
      });
      if (info.supported && info.powerEfficient) efficient.add(mime);
    } catch {}
  }
  codecEfficient = efficient;
  const rank = ({ mimeType: m, sdpFmtpLine: f = '' }) => {
    const base = efficient.has(m) ? 0 : 10;
    const order = { 'video/H264': 0, 'video/AV1': 1, 'video/VP9': 2, 'video/VP8': 3 }[m] ?? 50;
    const h264Mode = m === 'video/H264' && !f.includes('packetization-mode=1') ? 0.5 : 0;
    return (order === 50 ? 100 : base + order) + h264Mode;
  };
  codecPrefs = caps.map((c, i) => ({ c, i })).sort((a, b) => rank(a.c) - rank(b.c) || a.i - b.i).map(({ c }) => c);
  return codecPrefs;
}

function stopShare() {
  if (!screenStream) return;
  for (const track of screenStream.getTracks()) track.stop();
  for (const track of displayStream?.getTracks() ?? []) track.stop();
  nativeAudio?.stop();
  nativeAudio = null;
  displayStream = null;
  for (const peer of peers.values()) {
    for (const tx of peer.screenTx) {
      try { tx.stop(); } catch {}
    }
    peer.screenTx = [];
  }
  sfu?.unpublish();
  screenStream = null;
  send({ t: 'state', state: myState() });
  clearInterval(statsTimer);
  if (localTile) destroyTile(localTile);
  localTile = null;
  $('#shareLabel').textContent = 'Compartilhar tela';
  $('#share').classList.remove('active');
  updateCounters();
}

async function applyQuality() {
  updateUploadHint();
  if (!screenStream) return;
  const q = QUALITIES[qualityKey];
  const [video] = screenStream.getVideoTracks();
  await video?.applyConstraints({ frameRate: { ideal: q.fps, max: q.fps }, height: { ideal: q.height, max: q.height } }).catch(() => {});
  await sfu?.setQuality(q);
  for (const peer of peers.values()) {
    for (const tx of peer.screenTx) {
      if (tx.sender.track?.kind !== 'video') continue;
      const params = tx.sender.getParameters();
      if (!params.encodings?.length) continue;
      params.encodings[0].maxBitrate = q.bitrate;
      params.encodings[0].maxFramerate = q.fps;
      await tx.sender.setParameters(params).catch(() => {});
    }
  }
}

function updateCounters() {
  $('#peopleCount').textContent = `${peers.size + 1}/10`;
  renderPeople();
  updateUploadHint();
}

function renderPeople() {
  const rows = [
    { name: myName, you: true, sharing: !!screenStream },
    ...[...peers.values()].map((p) => ({ name: p.name, you: false, sharing: !!p.state.screen })),
  ];
  $('#peopleList').replaceChildren(
    ...rows.map(({ name, you, sharing }) => {
      const li = document.createElement('li');
      const avatar = document.createElement('span');
      avatar.className = 'avatar';
      avatar.textContent = (name.trim()[0] ?? '?').toUpperCase();
      avatar.style.background = toneFor(name);
      const who = document.createElement('span');
      who.className = 'who';
      who.textContent = name;
      if (you) {
        const tag = document.createElement('span');
        tag.className = 'you';
        tag.textContent = ' (você)';
        who.append(tag);
      }
      li.append(avatar, who);
      if (sharing) {
        const tag = document.createElement('span');
        tag.className = 'people-tag';
        tag.textContent = 'AO VIVO';
        li.append(tag);
      }
      return li;
    }),
  );
}

$('#viewers').addEventListener('click', (ev) => {
  ev.stopPropagation();
  const list = $('#peopleList');
  list.hidden = !list.hidden;
  $('#viewers').setAttribute('aria-expanded', String(!list.hidden));
});

document.addEventListener('click', (ev) => {
  const list = $('#peopleList');
  if (!list.hidden && !list.contains(ev.target)) {
    list.hidden = true;
    $('#viewers').setAttribute('aria-expanded', 'false');
  }
});

document.addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') closeSheet();
});

$('#share').addEventListener('click', () => (screenStream ? stopShare() : openSheet()));

$('#copyLink').addEventListener('click', async () => {
  const link = `${SERVER}/?r=${roomId}`;
  try {
    await navigator.clipboard.writeText(link);
    toast('Link copiado! Quem não tem o app entra pelo navegador ou baixa em telinha-xp0y.onrender.com/download', 4500);
  } catch {
    toast(link, 8000);
  }
});

$('#leave').addEventListener('click', () => {
  stopShare();
  for (const id of [...peers.keys()]) removePeer(id);
  myId = null;
  ws?.close();
  location.reload();
});

window.addEventListener('beforeunload', () => ws?.close());

const ICONS = {
  volume: '<svg viewBox="0 0 24 24"><path d="M11 5 6 9H2v6h4l5 4V5zM15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14"/></svg>',
  fullscreen: '<svg viewBox="0 0 24 24"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>',
};

const TONES = ['#f5f5f6', '#ff9b52', '#ff5fa8', '#47e08a', '#ffdcc6', '#7cc4ff', '#ff5f6d'];

function toneFor(name) {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.codePointAt(0)) >>> 0;
  return TONES[h % TONES.length];
}

function eyes({ w, h, gap, mood = 'look', color = '#f5f5f6' }) {
  const root = document.createElement('span');
  root.className = `eyes eyes-${mood}`;
  root.style.cssText = `--ew:${w}px;--eh:${h}px;--eg:${gap}px;--ec:${color};--amp:${Math.max(1.5, w * 0.35)}px`;
  const inner = document.createElement('span');
  inner.className = 'eyes-in';
  inner.append(document.createElement('i'), document.createElement('i'));
  for (const i of inner.children) i.className = 'eye';
  root.append(inner);
  return root;
}

const DEFAULT_TINT = [124, 196, 255];
const tintCanvas = document.createElement('canvas');
tintCanvas.width = tintCanvas.height = 12;
const tintCtx = tintCanvas.getContext('2d', { willReadFrequently: true });
let tintTimer = 0;

function startTintLoop() {
  if (tintTimer) return;
  tintTimer = setInterval(updateTint, 1200);
}

function updateTint() {
  const video = (focusedTile ?? localTile ?? [...peers.values()].find((p) => p.tile)?.tile)?.video;
  let rgb = DEFAULT_TINT;
  if (video && video.readyState >= 2 && video.videoWidth) {
    try {
      tintCtx.drawImage(video, 0, 0, 12, 12);
      rgb = vivid(averageColor(tintCtx.getImageData(0, 0, 12, 12).data));
    } catch {}
  }
  document.documentElement.style.setProperty('--tint', `rgb(${rgb.join(',')})`);
}

function averageColor(data) {
  let r = 0, g = 0, b = 0, wsum = 0;
  for (let i = 0; i < data.length; i += 4) {
    const max = Math.max(data[i], data[i + 1], data[i + 2]);
    const min = Math.min(data[i], data[i + 1], data[i + 2]);
    const w = 0.15 + (max - min) / 255;
    r += data[i] * w; g += data[i + 1] * w; b += data[i + 2] * w; wsum += w;
  }
  return [r / wsum, g / wsum, b / wsum];
}

function vivid([r, g, b]) {
  const max = Math.max(r, g, b, 1);
  const min = Math.min(r, g, b);
  if (max - min < 18) return DEFAULT_TINT;
  const scale = 235 / max;
  const mid = (r + g + b) / 3;
  return [r, g, b].map((c) => Math.round(Math.min(255, Math.max(0, (mid + (c - mid) * 1.6) * scale))));
}

(() => {
  const lobby = $('#lobby');
  const hero = lobby.querySelector('.hero-eyes .eyes-in');
  if (!hero) return;
  let idle = 0;
  lobby.addEventListener('pointermove', (ev) => {
    const r = hero.getBoundingClientRect();
    const dx = ev.clientX - (r.left + r.width / 2);
    const dy = ev.clientY - (r.top + r.height / 2);
    const d = Math.hypot(dx, dy) || 1;
    const k = Math.min(1, d / 260);
    hero.classList.add('follow');
    hero.style.transform = `translate(${(dx / d) * 7 * k}px, ${(dy / d) * 5 * k}px)`;
    clearTimeout(idle);
    idle = setTimeout(() => {
      hero.style.transform = '';
      hero.classList.remove('follow');
    }, 2200);
  });
})();

async function setupUpdates() {
  const version = await appVersion();
  if (version) $('#version').textContent = `v${version}`;
  const update = await checkUpdate();
  if (!update) return;
  const btn = $('#updateBtn');
  const label = $('#updateLabel');
  label.innerHTML = `Atualizar <span class="mono">v${update.version}</span>`;
  btn.hidden = false;
  btn.addEventListener('click', async () => {
    if (screenStream && !confirmLeave()) return;
    btn.disabled = true;
    label.textContent = 'Baixando…';
    try {
      await installUpdate(update, (pct) => (label.textContent = `Baixando ${pct}%`));
    } catch (err) {
      btn.disabled = false;
      label.innerHTML = `Atualizar <span class="mono">v${update.version}</span>`;
      toast(`Não deu pra atualizar: ${err}`);
    }
  }, { once: false });
}

function confirmLeave() {
  toast('Pare de transmitir antes de atualizar.', 3500);
  return false;
}

hardwareCodecPrefs();
