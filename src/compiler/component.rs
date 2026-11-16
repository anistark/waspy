//! A module that imports WASI preview 2 interfaces (`wasi:http`, through
//! `compiler::http`) made into a component, which is the only thing that can
//! import them: the core module gets component-type metadata for a world
//! importing `wasi:http/outgoing-handler` and `wasi:http/types`, and the
//! preview 1 adapter serves its `wasi_snapshot_preview1` imports (printing,
//! files, arguments, the clock) through preview 2. A module that imports no
//! `wasi:` interface is left as it is.
//!
//! The interfaces are WASI's own definitions (`src/compiler/wit/deps/`,
//! from the `wasip2` crate, Apache-2.0 WITH LLVM-exception).

use anyhow::{anyhow, Context, Result};

/// The WASI version the `wasi:` imports name.
pub const WASI_VERSION: &str = "0.2.12";

/// The packages, each after the ones it uses.
const PACKAGES: &[(&str, &str)] = &[
    ("io.wit", include_str!("wit/deps/io.wit")),
    ("clocks.wit", include_str!("wit/deps/clocks.wit")),
    ("random.wit", include_str!("wit/deps/random.wit")),
    ("filesystem.wit", include_str!("wit/deps/filesystem.wit")),
    ("sockets.wit", include_str!("wit/deps/sockets.wit")),
    ("cli.wit", include_str!("wit/deps/cli.wit")),
    ("http.wit", include_str!("wit/deps/http.wit")),
];

/// Whether a core module imports a WASI preview 2 interface.
pub fn imports_preview2(core: &[u8]) -> bool {
    for payload in wasmparser::Parser::new(0).parse_all(core) {
        if let Ok(wasmparser::Payload::ImportSection(reader)) = payload {
            for import in reader.into_iter().flatten() {
                if import.module.starts_with("wasi:") {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a core module exports `_start` (a command).
fn is_command(core: &[u8]) -> bool {
    for payload in wasmparser::Parser::new(0).parse_all(core) {
        if let Ok(wasmparser::Payload::ExportSection(reader)) = payload {
            for export in reader.into_iter().flatten() {
                if export.name == "_start" {
                    return true;
                }
            }
        }
    }
    false
}

/// `core` as a component: a command (exporting `wasi:cli/run`) when it
/// exports `_start`, a reactor otherwise.
pub fn componentize(core: &[u8]) -> Result<Vec<u8>> {
    use wasi_preview1_component_adapter_provider as adapter;
    // The definitions name interfaces behind unstable features (a time zone
    // clock), which nothing here imports.
    let mut resolve = wit_parser::Resolve {
        all_features: true,
        ..Default::default()
    };
    for (path, text) in PACKAGES {
        resolve
            .push_str(path, text)
            .with_context(|| format!("WASI's {path} does not resolve"))?;
    }
    let world = format!(
        "package waspy:program;\n\
         world program {{\n  \
           import wasi:http/types@{WASI_VERSION};\n  \
           import wasi:http/outgoing-handler@{WASI_VERSION};\n\
         }}\n"
    );
    let package = resolve
        .push_str("program.wit", &world)
        .context("the program's world does not resolve")?;
    let world = resolve.select_world(&[package], Some("program"))?;
    let mut module = core.to_vec();
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    let bytes = if is_command(core) {
        adapter::WASI_SNAPSHOT_PREVIEW1_COMMAND_ADAPTER
    } else {
        adapter::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER
    };
    wit_component::ComponentEncoder::default()
        .validate(true)
        .module(&module)?
        .adapter(adapter::WASI_SNAPSHOT_PREVIEW1_ADAPTER_NAME, bytes)?
        .encode()
        .map_err(|e| anyhow!("the module could not be made a component: {e:#}"))
}

#[cfg(test)]
mod tests {
    /// A command made a component runs under wasmtime, its output and exit
    /// status as the module's (skipped without wasmtime).
    #[test]
    fn a_command_component_runs() {
        let options = crate::CompilerOptions {
            optimize: false,
            ..crate::CompilerOptions::default()
        };
        let core = crate::compile_python_to_wasm_with_options(
            "import sys\nif __name__ == \"__main__\":\n    print(\"hi\", sys.argv[1:])\n",
            &options,
        )
        .unwrap();
        let component = super::componentize(&core).unwrap();
        let path =
            std::env::temp_dir().join(format!("waspy-component-{}.wasm", std::process::id()));
        std::fs::write(&path, &component).unwrap();
        let Ok(out) = std::process::Command::new("wasmtime")
            .arg("run")
            .arg(&path)
            .arg("a")
            .output()
        else {
            eprintln!("wasmtime not found; skipped");
            return;
        };
        let _ = std::fs::remove_file(&path);
        assert_eq!(String::from_utf8_lossy(&out.stderr), "");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi ['a']\n");
    }
}
