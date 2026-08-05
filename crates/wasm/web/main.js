import init, { render, serialize_timeline } from './cymbal_wasm.js';

const ctx = new AudioContext({ sampleRate: 48000 });
let node = null;
let ready = null;
let wasmBytes = null;
let engineSent = false;

const statusEl = document.getElementById('status');

function setStatus(text, error = false) {
  statusEl.textContent = text;
  statusEl.classList.toggle('error', error);
}

async function loadWasm() {
  if (!wasmBytes) {
    const res = await fetch('./cymbal_wasm_bg.wasm');
    if (!res.ok) throw new Error('engine fetch failed: ' + res.status);
    wasmBytes = await res.arrayBuffer();
  }
  return wasmBytes;
}

async function ensureReady() {
  if (!ready) ready = init({ module_or_path: await loadWasm() });
  return ready;
}

async function ensureNode() {
  if (node) return node;
  await ctx.resume();
  await ctx.audioWorklet.addModule('worklet.js');
  let created = null;
  for (let i = 0; i < 20 && !created; i++) {
    try {
      created = new AudioWorkletNode(ctx, 'cymbal', { outputChannelCount: [2] });
    } catch (e) {
      if (i === 19) throw e;
      await new Promise((r) => setTimeout(r, 50));
    }
  }
  node = created;
  node.connect(ctx.destination);
  node.port.onmessage = (e) => {
    const msg = e.data;
    if (msg && msg.type === 'error') {
      setStatus('engine error: ' + msg.message, true);
    } else if (msg && msg.type === 'ready') {
      setStatus('playing');
    }
  };
  return node;
}

async function play(src) {
  const bytes = await loadWasm();
  const n = await ensureNode();
  if (!engineSent) {
    const module = await WebAssembly.compile(bytes);
    n.port.postMessage({ type: 'init', module });
    engineSent = true;
  }
  const timeline = serialize_timeline(src, 3600);
  n.port.postMessage({ type: 'timeline', bytes: timeline }, [timeline.buffer]);
}

const srcInput = document.getElementById('src');
document.getElementById('play').onclick = async () => {
  setStatus('loading');
  try {
    await ensureReady();
    await play(srcInput.value);
  } catch (e) {
    setStatus(String(e), true);
  }
};
document.getElementById('render').onclick = async () => {
  setStatus('rendering');
  try {
    await ensureReady();
    const seconds = 4;
    const out = render(srcInput.value, seconds);
    const wav = new Uint8Array(44 + out.length * 4);
    const dv = new DataView(wav.buffer);
    dv.setUint32(0, 0x46464952, true); // RIFF
    dv.setUint32(4, 36 + out.length * 4, true);
    wav.set([0x57, 0x41, 0x56, 0x45], 8);
    wav.set([0x66, 0x6d, 0x74, 0x20], 12);
    dv.setUint32(16, 16, true);
    dv.setUint16(20, 3, true); // float
    dv.setUint16(22, 2, true);
    dv.setUint32(24, 48000, true);
    dv.setUint32(28, 48000 * 2 * 4, true);
    dv.setUint16(32, 8, true);
    dv.setUint16(34, 32, true);
    wav.set([0x64, 0x61, 0x74, 0x61], 36);
    dv.setUint32(40, out.length * 4, true);
    for (let i = 0; i < out.length; i++) dv.setFloat32(44 + i * 4, out[i], true);
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([wav], { type: 'audio/wav' }));
    a.download = 'cymbal.wav';
    a.click();
    setStatus('rendered cymbal.wav');
  } catch (e) {
    setStatus(String(e), true);
  }
};
