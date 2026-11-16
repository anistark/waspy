//! `waspy`: compile Python to WebAssembly from the command line.
//!
//! ```text
//! waspy build app.py [-o app.wasm] [--no-optimize] [--js]
//! waspy check app.py
//! waspy run app.py [--runtime wasmtime] [-- args...]
//! ```
//!
//! A path is a `.py` file (the modules it imports beside it are compiled
//! with it) or a project directory. Errors are printed as
//! `path:line:column: error: message`, the form editors read.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use waspy::{CompilerOptions, Verbosity};

const USAGE: &str = "\
waspy compiles Python to WebAssembly.

Usage:
  waspy build <path> [-o <out.wasm>] [--no-optimize] [--js] [-v]
  waspy check <path>
  waspy run <path> [--runtime <command>] [-- <args>...]
  waspy --version
  waspy help

<path> is a .py file (the modules it imports beside it are compiled with it)
or a project directory.

build   compile to WebAssembly; the output is <path>.wasm unless -o names it.
        --js wraps every export for the JavaScript loader and writes
        waspy-loader.js beside the module.
check   compile without writing anything: report what the compiler refuses,
        each error with its file, line, and column, and exit with status 1
        if there is any.
run     compile, then run the module with a WebAssembly runtime (wasmtime by
        default, or the --runtime command), with the current directory
        preopened and <args> as sys.argv[1:]; a program making HTTP requests
        runs with wasmtime's HTTP support turned on.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") | Some("version") => {
            println!("waspy {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("build") => build(&args[1..]),
        Some("check") => check(&args[1..]),
        Some("run") => run(&args[1..]),
        Some(other) => {
            eprintln!("waspy: unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The options and operands of a command.
struct Parsed {
    path: PathBuf,
    out: Option<PathBuf>,
    optimize: bool,
    js: bool,
    verbose: bool,
    runtime: Option<String>,
    rest: Vec<String>,
}

fn parse(args: &[String], command: &str) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        path: PathBuf::new(),
        out: None,
        optimize: true,
        js: false,
        verbose: false,
        runtime: None,
        rest: Vec::new(),
    };
    let mut path = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--" => {
                parsed.rest = args[i + 1..].to_vec();
                break;
            }
            "-o" | "--output" => {
                i += 1;
                let out = args
                    .get(i)
                    .ok_or_else(|| format!("waspy {command}: -o needs a file name"))?;
                parsed.out = Some(PathBuf::from(out));
            }
            "--no-optimize" => parsed.optimize = false,
            "--js" => parsed.js = true,
            "-v" | "--verbose" => parsed.verbose = true,
            "--runtime" => {
                i += 1;
                let runtime = args
                    .get(i)
                    .ok_or_else(|| format!("waspy {command}: --runtime needs a command"))?;
                parsed.runtime = Some(runtime.clone());
            }
            _ if arg.starts_with('-') => {
                return Err(format!("waspy {command}: unknown option '{arg}'"));
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            // `waspy run app.py a b`: the program's own arguments.
            _ if command == "run" => {
                parsed.rest = args[i..].to_vec();
                break;
            }
            _ => return Err(format!("waspy {command}: unexpected argument '{arg}'")),
        }
        i += 1;
    }
    parsed.path = path.ok_or_else(|| format!("waspy {command}: which file? (waspy help)"))?;
    Ok(parsed)
}

/// Compile `parsed.path` (a file or a project directory).
fn compile(parsed: &Parsed) -> Result<Vec<u8>, String> {
    let options = CompilerOptions {
        optimize: parsed.optimize,
        verbosity: if parsed.verbose {
            Verbosity::Verbose
        } else {
            Verbosity::Quiet
        },
        js_exports: parsed.js,
        ..CompilerOptions::default()
    };
    let path = &parsed.path;
    if !path.exists() {
        return Err(format!("{}: no such file or directory", path.display()));
    }
    let result = if path.is_dir() {
        waspy::compile_python_project_with_options(path, &options)
    } else {
        waspy::compile_python_file_with_options(path, &options)
    };
    result.map_err(|e| located(path, &e))
}

/// An error as `path:line:column: error: message`, where the compiler gave
/// the place, and with the function it was found in otherwise.
fn located(path: &Path, error: &anyhow::Error) -> String {
    // The deepest cause is the compiler's own message; the outer ones are
    // the stage it failed in.
    let messages: Vec<String> = error.chain().map(|e| e.to_string()).collect();
    let message = messages.last().cloned().unwrap_or_default();
    let mut out = Vec::new();
    // One run can report several errors, each on its own line, the later
    // ones after "also:".
    for (k, line) in message.lines().enumerate() {
        let line = line.trim().trim_start_matches("also: ");
        if line.is_empty() {
            continue;
        }
        let shown = place(path, line);
        if k == 0 || !shown.is_empty() {
            out.push(shown);
        }
    }
    out.join("\n")
}

/// One error line, with its file, line, and column moved to the front.
fn place(path: &Path, line: &str) -> String {
    let file = path.display();
    // "... (at line 3, column 5)" or "... at line 3, column 5"
    if let Some(at) = line.rfind("at line ") {
        let tail = &line[at + "at line ".len()..];
        let number: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        let column: String = tail
            .split("column ")
            .nth(1)
            .map(|c| c.chars().take_while(|c| c.is_ascii_digit()).collect())
            .unwrap_or_default();
        if !number.is_empty() {
            let mut text = line[..at].trim_end().to_string();
            if text.ends_with('(') {
                text.pop();
            }
            let text = text.trim_end().trim_start_matches("Unsupported feature: ");
            let col = if column.is_empty() {
                String::new()
            } else {
                format!("{column}:")
            };
            return format!("{file}:{number}:{col} error: {text}");
        }
    }
    let text = line.trim_start_matches("Unsupported feature: ");
    // "... (in function 'f')": the line of that function's `def`.
    if let Some(start) = line.rfind("(in function '") {
        let name = &line[start + "(in function '".len()..];
        let name = name.split('\'').next().unwrap_or("");
        let short = name.rsplit('.').next().unwrap_or(name);
        if let Some(number) = def_line(path, short) {
            return format!("{file}:{number}: error: {text}");
        }
    }
    format!("{file}: error: {text}")
}

/// The line of `def name(` in the file at `path`, when there is one.
fn def_line(path: &Path, name: &str) -> Option<usize> {
    if name.is_empty() || name.starts_with("__") && name.ends_with("__") && name != "__init__" {
        return None;
    }
    let source = std::fs::read_to_string(path).ok()?;
    let needle = format!("def {name}(");
    source
        .lines()
        .position(|l| {
            l.trim_start().starts_with(&needle)
                || l.trim_start().starts_with(&format!("async {needle}"))
        })
        .map(|i| i + 1)
}

fn output_path(parsed: &Parsed) -> PathBuf {
    if let Some(out) = &parsed.out {
        return out.clone();
    }
    let path = &parsed.path;
    if path.is_dir() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "module".to_string());
        path.join(format!("{name}.wasm"))
    } else {
        path.with_extension("wasm")
    }
}

fn build(args: &[String]) -> ExitCode {
    let parsed = match parse(args, "build") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let wasm = match compile(&parsed) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let out = output_path(&parsed);
    if let Err(e) = std::fs::write(&out, &wasm) {
        eprintln!("waspy build: cannot write {}: {e}", out.display());
        return ExitCode::from(1);
    }
    if parsed.js {
        let loader = out
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("waspy-loader.js");
        if let Err(e) = std::fs::write(&loader, waspy::js_loader()) {
            eprintln!("waspy build: cannot write {}: {e}", loader.display());
            return ExitCode::from(1);
        }
    }
    eprintln!("wrote {} ({} bytes)", out.display(), wasm.len());
    ExitCode::SUCCESS
}

fn check(args: &[String]) -> ExitCode {
    let mut parsed = match parse(args, "check") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    // Checking does not need the optimizer.
    parsed.optimize = false;
    match compile(&parsed) {
        Ok(_) => {
            eprintln!("{}: ok", parsed.path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> ExitCode {
    let mut parsed = match parse(args, "run") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    // Starting quickly matters more than size for a run.
    parsed.optimize = false;
    let wasm = match compile(&parsed) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let stem = parsed
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "module".to_string());
    let module = std::env::temp_dir().join(format!("waspy-run-{}-{stem}.wasm", std::process::id()));
    if let Err(e) = std::fs::write(&module, &wasm) {
        eprintln!("waspy run: cannot write {}: {e}", module.display());
        return ExitCode::from(1);
    }
    let runtime = parsed
        .runtime
        .clone()
        .or_else(|| std::env::var("WASPY_RUNTIME").ok())
        .unwrap_or_else(|| "wasmtime".to_string());
    let mut command = std::process::Command::new(&runtime);
    if Path::new(&runtime)
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("wasmtime"))
    {
        command.args(["run", "--dir=."]);
        // A program making HTTP requests is a component importing
        // `wasi:http`, which wasmtime provides when asked.
        if wasm.get(4..8) == Some(&[0x0d, 0x00, 0x01, 0x00]) {
            command.args(["-S", "http"]);
        }
    }
    command.arg(&module).args(&parsed.rest);
    let status = command.status();
    let _ = std::fs::remove_file(&module);
    match status {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(e) => {
            eprintln!(
                "waspy run: cannot start '{runtime}': {e}. Install wasmtime \
                 (https://wasmtime.dev), or name another runtime with --runtime \
                 or WASPY_RUNTIME"
            );
            ExitCode::from(1)
        }
    }
}
