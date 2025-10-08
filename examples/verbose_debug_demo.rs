use waspy::core::options::Verbosity;
use waspy::{compile_python_to_wasm_with_options, CompilerOptions};

fn main() -> anyhow::Result<()> {
    let python_source = r#"
def add(a: int, b: int) -> int:
    return a + b

def multiply(x: int, y: int) -> int:
    result = x * y
    return result

@wasm_export
def calculate(n: int) -> int:
    return add(n, multiply(n, 2))
"#;

    println!("=== Compilation with Normal verbosity ===");
    println!();
    let options_normal = CompilerOptions {
        verbosity: Verbosity::Normal,
        ..CompilerOptions::default()
    };
    let wasm_normal = compile_python_to_wasm_with_options(python_source, &options_normal)?;
    println!("Generated WASM size: {} bytes\n", wasm_normal.len());

    println!("\n=== Compilation with Verbose output (--verbose) ===");
    println!();
    let options_verbose = CompilerOptions {
        verbosity: Verbosity::Verbose,
        ..CompilerOptions::default()
    };
    let wasm_verbose = compile_python_to_wasm_with_options(python_source, &options_verbose)?;
    println!("Generated WASM size: {} bytes\n", wasm_verbose.len());

    println!("\n=== Compilation with Debug output (--debug) ===");
    println!();
    let options_debug = CompilerOptions {
        verbosity: Verbosity::Debug,
        ..CompilerOptions::default()
    };
    let wasm_debug = compile_python_to_wasm_with_options(python_source, &options_debug)?;
    println!("Generated WASM size: {} bytes\n", wasm_debug.len());

    println!("\n=== Using from_flags helper ===");
    println!();
    // Simulate command-line flags
    let verbose = true;
    let debug = false;
    let options_from_flags = CompilerOptions {
        verbosity: Verbosity::from_flags(verbose, debug),
        ..CompilerOptions::default()
    };
    let wasm_from_flags = compile_python_to_wasm_with_options(python_source, &options_from_flags)?;
    println!("Generated WASM size: {} bytes\n", wasm_from_flags.len());

    println!("\n=== Example: How a CLI tool would use this ===");
    println!("In your CLI parser, you would:");
    println!("1. Parse --verbose and --debug flags from command line");
    println!("2. Create CompilerOptions with: Verbosity::from_flags(verbose, debug)");
    println!("3. Call compile_python_to_wasm_with_options with those options");
    println!();
    println!("Example pseudo-code:");
    println!("  let matches = clap::App::new(\"my-compiler\")");
    println!("      .arg(Arg::with_name(\"verbose\").long(\"verbose\"))");
    println!("      .arg(Arg::with_name(\"debug\").long(\"debug\"))");
    println!("      .get_matches();");
    println!("  ");
    println!("  let options = CompilerOptions {{");
    println!("      verbosity: Verbosity::from_flags(");
    println!("          matches.is_present(\"verbose\"),");
    println!("          matches.is_present(\"debug\")");
    println!("      ),");
    println!("      ..CompilerOptions::default()");
    println!("  }};");

    Ok(())
}
