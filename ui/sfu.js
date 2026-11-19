const waitGathering = (pc, ms) =>
  new Promise((resolve) => {
    if (pc.iceGatheringState === 'complete') return resolve();
    const timer = setTimeout(resolve, ms);
    pc.addEventListener('icegatheringstatechange', () => {
      if (pc.iceGatheringState !== 'complete') return;
      clearTimeout(timer);
      resolve();
    });
  });

export function createSfu({ send, iceServers, onStream, onStreamGone, onState }) {
  let up = null;
  let down = null;
  let downQueue = Promise.resolve();
  const streams = new Map();

  function ensureDown() {
    if (down) return down;
    down = new RTCPeerConnection({ iceServers });
    down.ontrack = ({ streams: [stream] }) => {
      const owner = stream?.id;
      if (!owner) return;
      if (streams.get(owner) !== stream) {
        streams.set(owner, stream);
        stream.addEventListener('removetrack', () => {
          if (streams.get(owner) === stream && stream.getVideoTracks().length === 0) {
            streams.delete(owner);
            onStreamGone(owner);
          }
        });
      }
      onStream(owner, stream);
    };
    down.onconnectionstatechange = () => onState?.(down.connectionState);
    return down;
  }

  function handle(msg) {
    if (msg.kind === 'up-answer') {
      up?.setRemoteDescription(msg.sdp).catch((err) => console.warn('sfu up', err));
    } else if (msg.kind === 'down-offer') {
      downQueue = downQueue
        .then(async () => {
          const pc = ensureDown();
          await pc.setRemoteDescription(msg.sdp);
          await pc.setLocalDescription();
          await waitGathering(pc, 250);
          send({ t: 'sfu', kind: 'down-answer', sdp: pc.localDescription });
        })
        .catch((err) => console.warn('sfu down', err));
    }
  }

  async function publish(stream, quality, codecPrefs) {
    unpublish(false);
    const pc = new RTCPeerConnection({ iceServers });
    up = pc;
    for (const track of stream.getTracks()) {
      const video = track.kind === 'video';
      const tx = pc.addTransceiver(track, {
        direction: 'sendonly',
        streams: [stream],
        ...(video && { sendEncodings: [{ maxBitrate: quality.bitrate, maxFramerate: quality.fps }] }),
      });
      if (video && codecPrefs?.length) {
        try {
          tx.setCodecPreferences(codecPrefs);
        } catch {}
      }
    }
    await pc.setLocalDescription();
    await waitGathering(pc, 250);
    if (up === pc) send({ t: 'sfu', kind: 'up-offer', sdp: pc.localDescription });
  }

  async function setQuality(quality) {
    for (const sender of up?.getSenders() ?? []) {
      if (sender.track?.kind !== 'video') continue;
      const params = sender.getParameters();
      if (!params.encodings?.length) continue;
      params.encodings[0].maxBitrate = quality.bitrate;
      params.encodings[0].maxFramerate = quality.fps;
      await sender.setParameters(params).catch(() => {});
    }
  }

  function unpublish(notify = true) {
    if (!up) return;
    up.close();
    up = null;
    if (notify) send({ t: 'sfu', kind: 'up-close' });
  }

  function close() {
    up?.close();
    down?.close();
    up = null;
    down = null;
    streams.clear();
  }

  return {
    handle,
    publish,
    setQuality,
    unpublish,
    close,
    videoSenders: () => up?.getSenders().filter((sender) => sender.track?.kind === 'video') ?? [],
    get state() {
      return down?.connectionState ?? 'new';
    },
  };
}
