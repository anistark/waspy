//! Waspy compiled to WebAssembly for the docs-site playground
//! (`docs/playground/`). Build it with `just playground`.

use std::sync::Mutex;

use wasm_bindgen::prelude::*;
use waspy::{CompilerOptions, Verbosity};

/// The message of the last panic. A panic aborts the call as a trap, so the
/// page reads this afterwards to report the compiler crash.
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut slot) = LAST_PANIC.lock() {
            *slot = Some(info.to_string());
        }
    }));
}

/// Compile Python source to an unoptimized WebAssembly module. The page runs
/// Binaryen itself (binaryen.js) when the optimize switch is on.
#[wasm_bindgen]
pub fn compile(source: &str) -> Result<Vec<u8>, JsError> {
    let options = CompilerOptions {
        optimize: false,
        verbosity: Verbosity::Quiet,
    };
    waspy::compile_python_to_wasm_with_options(source, &options).map_err(to_js_error)
}

/// The signature of every top-level function, as a JSON array of
/// `{"name", "params": ["n: int", ...], "returns"}`.
#[wasm_bindgen]
pub fn signatures(source: &str) -> Result<String, JsError> {
    let sigs = waspy::get_python_file_metadata(source).map_err(to_js_error)?;
    let rows: Vec<serde_json::Value> = sigs
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "params": s.parameters,
                "returns": s.return_type,
            })
        })
        .collect();
    Ok(serde_json::Value::Array(rows).to_string())
}

/// Take the message of the panic that trapped the last call, if any.
#[wasm_bindgen]
pub fn take_panic() -> Option<String> {
    LAST_PANIC.lock().ok().and_then(|mut slot| slot.take())
}

fn to_js_error(err: anyhow::Error) -> JsError {
    JsError::new(&format!("{err:#}"))
}
