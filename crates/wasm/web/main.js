import { compile_events, render, serialize_timeline } from './cymbal_wasm.js';

const ctx = new AudioContext({ sampleRate: 48000 });
let node = null;

async function play(src) {
  if (!node) {
    await ctx.audioWorklet.addModule('worklet.js');
    node = new AudioWorkletNode(ctx, 'cymbal');
    node.connect(ctx.destination);
  }
  const bytes = serialize_timeline(src, 3600);
  node.port.postMessage(bytes);
}

const srcInput = document.getElementById('src');
document.getElementById('play').onclick = async () => {
  await play(srcInput.value);
};
document.getElementById('render').onclick = () => {
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
};
