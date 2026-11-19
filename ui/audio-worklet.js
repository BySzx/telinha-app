const CHANNELS = 2;
const CAPACITY = 48000 * CHANNELS;
const PREBUFFER = Math.round(48000 * 0.04) * CHANNELS;
const MAX_BUFFERED = Math.round(48000 * 0.25) * CHANNELS;
const TARGET_BUFFERED = Math.round(48000 * 0.06) * CHANNELS;

class PcmPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ring = new Float32Array(CAPACITY);
    this.read = 0;
    this.write = 0;
    this.size = 0;
    this.playing = false;
    this.port.onmessage = (ev) => this.push(new Float32Array(ev.data));
  }

  push(samples) {
    for (let i = 0; i < samples.length; i++) {
      this.ring[this.write] = samples[i];
      this.write = (this.write + 1) % CAPACITY;
      if (this.size < CAPACITY) this.size++;
      else this.read = (this.read + 1) % CAPACITY;
    }
    if (this.size > MAX_BUFFERED) {
      const drop = this.size - TARGET_BUFFERED;
      this.read = (this.read + drop) % CAPACITY;
      this.size -= drop;
    }
  }

  process(_inputs, outputs) {
    const [left, right] = outputs[0];
    if (!this.playing && this.size >= PREBUFFER) this.playing = true;
    for (let i = 0; i < left.length; i++) {
      if (this.playing && this.size >= CHANNELS) {
        left[i] = this.ring[this.read];
        right[i] = this.ring[(this.read + 1) % CAPACITY];
        this.read = (this.read + CHANNELS) % CAPACITY;
        this.size -= CHANNELS;
      } else {
        left[i] = 0;
        right[i] = 0;
        this.playing = false;
      }
    }
    return true;
  }
}

registerProcessor('pcm-player', PcmPlayer);
