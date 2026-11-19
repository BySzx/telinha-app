const tauri = window.__TAURI__;

export const isNative = Boolean(tauri?.core);

export async function listAudioApps() {
  if (!isNative) return [];
  return tauri.core.invoke('list_audio_apps');
}

export async function startNativeAudio(source) {
  const ctx = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
  await ctx.audioWorklet.addModule('audio-worklet.js');
  const node = new AudioWorkletNode(ctx, 'pcm-player', {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [2],
  });
  const destination = ctx.createMediaStreamDestination();
  destination.channelCount = 2;
  node.connect(destination);

  const channel = new tauri.core.Channel();
  channel.onmessage = (data) => {
    const buffer = data instanceof ArrayBuffer ? data : new Uint8Array(data).buffer;
    node.port.postMessage(buffer, [buffer]);
  };

  try {
    await tauri.core.invoke('start_audio', { source, onData: channel });
  } catch (err) {
    await ctx.close();
    throw err;
  }
  await ctx.resume();

  const [track] = destination.stream.getAudioTracks();
  track.contentHint = 'music';
  let stopped = false;
  return {
    track,
    async stop() {
      if (stopped) return;
      stopped = true;
      track.stop();
      node.disconnect();
      await tauri.core.invoke('stop_audio').catch(() => {});
      await ctx.close().catch(() => {});
    },
  };
}

export function bindWindowControls() {
  if (!tauri?.window) return;
  const win = tauri.window.getCurrentWindow();
  document.querySelectorAll('[data-win]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const action = btn.dataset.win;
      if (action === 'close') win.close();
      else if (action === 'minimize') win.minimize();
      else if (action === 'maximize') win.toggleMaximize();
    });
  });
}

export async function appVersion() {
  return tauri?.app ? tauri.app.getVersion() : null;
}

export async function checkUpdate() {
  if (!tauri?.updater) return null;
  try {
    return await tauri.updater.check();
  } catch {
    return null;
  }
}

export async function installUpdate(update, onProgress) {
  let total = 0;
  let done = 0;
  await update.downloadAndInstall((ev) => {
    if (ev.event === 'Started') total = ev.data.contentLength ?? 0;
    else if (ev.event === 'Progress') {
      done += ev.data.chunkLength;
      if (total) onProgress(Math.min(100, Math.round((done / total) * 100)));
    }
  });
  await tauri.process.relaunch();
}
