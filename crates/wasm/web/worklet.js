let exp = null;
let eng = 0;
let mem = null;
let outPtr = 0;

async function loadEngine() {
  const res = await fetch('./cymbal_wasm_bg.wasm');
  const bytes = await res.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {
    wbg: {
      __wbindgen_error_new: (ptr, len) => {
        const view = new Uint8Array(mem.buffer, ptr, len);
        let s = '';
        for (let i = 0; i < len; i++) s += String.fromCharCode(view[i]);
        return new Error(s);
      },
      __wbindgen_init_externref_table: () => {
        const table = exp.__wbindgen_export_0;
        const offset = table.grow(4);
        table.set(0, undefined);
        table.set(offset + 0, undefined);
        table.set(offset + 1, null);
        table.set(offset + 2, true);
        table.set(offset + 3, false);
      },
    },
  });
  exp = instance.exports;
  mem = exp.memory;
  exp.__wbindgen_start();
  eng = exp.eng_alloc(96000n, 48000);
  outPtr = exp.eng_out_ptr();
  self.eng = exp;
}

class CymbalProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ready = loadEngine().then(() => true);
    this.port.onmessage = (e) => {
      const bytes = new Uint8Array(e.data);
      const ptr = self.eng.eng_in_ptr(bytes.length);
      new Uint8Array(mem.buffer).set(bytes, ptr);
      self.eng.eng_submit(eng, ptr, bytes.length);
    };
  }
  process(inputs, outputs) {
    const out = outputs[0];
    const frames = out[0].length;
    if (!self.eng) return true;
    const memF32 = new Float32Array(mem.buffer);
    self.eng.eng_process(eng, outPtr, frames);
    for (let i = 0; i < frames; i++) {
      out[0][i] = memF32[outPtr / 4 + i * 2];
      out[1][i] = memF32[outPtr / 4 + i * 2 + 1];
    }
    return true;
  }
}
registerProcessor('cymbal', CymbalProcessor);
