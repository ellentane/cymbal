let exp = null;
let eng = 0;
let mem = null;
let outPtr = 0;
let pending = null;

function submit(bytes) {
  const ptr = exp.eng_in_ptr(bytes.length);
  new Uint8Array(mem.buffer).set(bytes, ptr);
  exp.eng_submit(eng, ptr, bytes.length);
}

function initEngine(module) {
  return WebAssembly.instantiate(module, {
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
  }).then((res) => {
    const inst = res instanceof WebAssembly.Instance ? res : res.instance;
    exp = inst.exports;
    mem = exp.memory;
    exp.__wbindgen_start();
    eng = exp.eng_alloc(96000n, 48000);
    outPtr = exp.eng_out_ptr();
    if (pending) {
      submit(pending);
      pending = null;
    }
  });
}

class CymbalProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.port.onmessage = (e) => {
      const msg = e.data;
      if (!msg || typeof msg !== 'object') return;
      if (msg.type === 'init') {
        initEngine(msg.module)
          .then(() => this.port.postMessage({ type: 'ready' }))
          .catch((err) => this.port.postMessage({ type: 'error', message: String(err) }));
      } else if (msg.type === 'timeline') {
        const bytes = new Uint8Array(msg.bytes);
        if (exp) {
          submit(bytes);
        } else {
          pending = bytes;
        }
      }
    };
  }
  process(inputs, outputs) {
    const out = outputs[0];
    const frames = out[0].length;
    if (!exp) return true;
    const memF32 = new Float32Array(mem.buffer);
    exp.eng_process(eng, outPtr, frames);
    for (let i = 0; i < frames; i++) {
      out[0][i] = memF32[outPtr / 4 + i * 2];
      out[1][i] = memF32[outPtr / 4 + i * 2 + 1];
    }
    return true;
  }
}
registerProcessor('cymbal', CymbalProcessor);
