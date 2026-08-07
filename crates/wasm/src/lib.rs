use cymbal_core::lexer::lex;
use cymbal_core::parser::parse;
use cymbal_core::render::render_offline;
use cymbal_core::scheduler::{SampleData, schedule};
use std::collections::HashMap;
use std::sync::Arc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

pub mod engine;

fn to_js_err(e: cymbal_core::error::Error) -> JsError {
    JsError::new(&e.to_string())
}

#[wasm_bindgen]
pub fn render(src: &str, seconds: u32) -> Result<Vec<f32>, JsError> {
    render_offline(src, seconds as u64 * 48000, 48000, &HashMap::new()).map_err(to_js_err)
}

/// Stores raw f32 frames (little-endian bytes) at registry `id` in this
/// instance's memory, mirroring `engine::eng_load_sample` on the main thread.
#[wasm_bindgen]
pub fn load_sample(id: u32, bytes: &[u8]) -> Result<(), JsError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(JsError::new("sample data must be f32 frames"));
    }
    let frames = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    engine::register_sample(id, frames, 48000);
    Ok(())
}

/// `samples` is a JS object mapping sample paths to registry ids, e.g.
/// `{ "kick.wav": 0 }`; ids the engine has no data for surface as the
/// scheduler's "sample 'path' not loaded" error.
#[wasm_bindgen]
pub fn serialize_timeline(src: &str, seconds: u32, samples: &JsValue) -> Result<Vec<u8>, JsError> {
    let program = parse(&lex(src).map_err(to_js_err)?).map_err(to_js_err)?;
    let mut ids: HashMap<String, u16> = HashMap::new();
    if samples.is_object() {
        let obj: &js_sys::Object = samples.unchecked_ref();
        for key in js_sys::Object::keys(obj).iter() {
            let Some(path) = key.as_string() else {
                continue;
            };
            let Some(id) = js_sys::Reflect::get(obj, &key)
                .ok()
                .and_then(|v| v.as_f64())
                .map(|f| f as u16)
            else {
                continue;
            };
            ids.insert(path, id);
        }
    }
    let mut sample_map: HashMap<String, Arc<SampleData>> = HashMap::new();
    for (path, id) in &ids {
        if let Some(data) = engine::registered_sample(*id) {
            sample_map.insert(path.clone(), data);
        }
    }
    let tl = schedule(
        &program,
        &HashMap::new(),
        &sample_map,
        seconds as u64 * 48000,
        48000,
    )
    .map_err(to_js_err)?;
    Ok(engine::serialize(&tl, &sample_map, &ids))
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    const SRC: &str = "tempo 120\nlet kick = kick()\nlet hat = hat()\nloop \"b\":\n    kick << \"x . . x\"\n    hat << \"x . x .\"\n";

    #[wasm_bindgen_test]
    fn wasm_render_matches_native() {
        let wasm = render(SRC, 1).unwrap();
        let native = render_offline(SRC, 48000, 48000, &HashMap::new()).unwrap();
        assert_eq!(wasm, native, "wasm render must equal native render");
    }

    #[wasm_bindgen_test]
    fn serialize_round_trips() {
        let bytes = serialize_timeline(SRC, 4, &JsValue::UNDEFINED).unwrap();
        let events = engine::deserialize_events(&bytes).unwrap();
        // 4s at 120bpm = 2 bars: kick "x . . x" -> 4 kicks, hat "x . x ." -> 4 hats
        assert_eq!(events.len(), 8);
        assert_eq!(events[0].voice, cymbal_core::ast::VoiceKind::Kick as u8);
        assert_eq!(events[1].voice, cymbal_core::ast::VoiceKind::Hat as u8);
    }

    #[wasm_bindgen_test]
    fn input_buffer_pointer_is_stable_across_growth() {
        let p1 = engine::eng_in_ptr(64);
        let p2 = engine::eng_in_ptr(128);
        let p3 = engine::eng_in_ptr(32);
        assert_eq!(p1, p2, "growth must reuse the buffer (no leak)");
        assert_eq!(p1, p3, "shrink keeps the same allocation");
    }

    #[wasm_bindgen_test]
    fn engine_smoke() {
        unsafe {
            let e = engine::eng_alloc(24000, 48000);
            let bytes = serialize_timeline(SRC, 1, &JsValue::UNDEFINED).unwrap();
            engine::eng_submit(e, bytes.as_ptr(), bytes.len());
            let mut out = vec![0.0f32; 48000 * 2];
            engine::eng_process(e, out.as_mut_ptr(), 48000);
            assert!(out[0..16].iter().any(|s| *s != 0.0), "engine renders");
            assert!(out.iter().all(|s| s.is_finite()));
            engine::eng_free(e);
        }
    }

    #[wasm_bindgen_test]
    fn engine_rejects_zero_bar_samples() {
        unsafe {
            let e = engine::eng_alloc(24000, 48000);
            let bytes = [0u8; 16]; // bar_samples 0, count 0
            engine::eng_submit(e, bytes.as_ptr(), bytes.len());
            let mut out = vec![1.0f32; 128 * 2];
            engine::eng_process(e, out.as_mut_ptr(), 128);
            assert!(out.iter().all(|s| *s == 0.0));
            engine::eng_free(e);
        }
    }

    #[wasm_bindgen_test]
    fn eng_alloc_zero_bar_samples_does_not_hang() {
        unsafe {
            let e = engine::eng_alloc(0, 48000);
            let mut out = vec![0.0f32; 128 * 2];
            engine::eng_process(e, out.as_mut_ptr(), 128);
            assert!(out.iter().all(|s| s.is_finite()));
            engine::eng_free(e);
        }
    }

    #[wasm_bindgen_test]
    fn engine_clamps_non_finite_wire_values() {
        unsafe {
            let e = engine::eng_alloc(24000, 48000);
            let mut bytes = serialize_timeline(SRC, 1, &JsValue::UNDEFINED).unwrap();
            bytes[16 + 13..16 + 17].copy_from_slice(&f32::NAN.to_le_bytes());
            bytes[16 + 25..16 + 29].copy_from_slice(&f32::INFINITY.to_le_bytes());
            engine::eng_submit(e, bytes.as_ptr(), bytes.len());
            let mut out = vec![0.0f32; 128 * 2];
            engine::eng_process(e, out.as_mut_ptr(), 128);
            assert!(
                out.iter().all(|s| s.is_finite()),
                "NaN velocity/pan must be clamped"
            );
            engine::eng_free(e);
        }
    }

    #[wasm_bindgen_test]
    fn eng_process_clamps_frames_to_scratch_size() {
        unsafe {
            let e = engine::eng_alloc(24000, 48000);
            let bytes = serialize_timeline(SRC, 1, &JsValue::UNDEFINED).unwrap();
            engine::eng_submit(e, bytes.as_ptr(), bytes.len());
            let mut out = vec![f32::NAN; 256 * 2];
            engine::eng_process(e, out.as_mut_ptr(), 256);
            assert!(
                out[256..512].iter().all(|s| s.is_nan()),
                "frames beyond 128 must not be written"
            );
            engine::eng_free(e);
        }
    }

    #[wasm_bindgen_test]
    fn deserialize_events_rejects_malformed_input() {
        assert!(engine::deserialize_events(&[]).is_err());
        assert!(engine::deserialize_events(&[0u8; 16]).unwrap().is_empty());
        assert!(engine::deserialize_events(&[0u8; 20]).is_err());
        let mut bytes = vec![0u8; 16 + 64];
        bytes[8..16].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(engine::deserialize_events(&bytes).unwrap().len(), 1);
        let mut bytes = vec![0u8; 16];
        bytes[8..16].copy_from_slice(&1_000_000u64.to_le_bytes());
        assert!(engine::deserialize_events(&bytes).is_err());
    }
}
