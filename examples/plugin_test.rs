//! Test the wasmrun plugin functionality
//!
//! This example demonstrates how to use the waspy plugin interface
//! without requiring wasmrun to be installed.

#[cfg(feature = "wasm-plugin")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use waspy::wasmrun::Plugin;
    use waspy::wasmrun::{BuildConfig, OptimizationLevel, WaspyPlugin};

    println!("🧪 Testing Waspy Plugin Integration");
    println!("===================================\n");

    // Create plugin instance
    let plugin = WaspyPlugin::new();
    let info = plugin.info();

    println!("Plugin Info:");
    println!("  Name: {}", info.name);
    println!("  Version: {}", info.version);
    println!("  Description: {}", info.description);
    println!("  Author: {}", info.author);
    println!("  Extensions: {:?}", info.extensions);
    println!("  Entry Files: {:?}", info.entry_files);
    println!("  Dependencies: {:?}", info.dependencies);
    println!("  Capabilities:");
    println!("    - Compile WASM: {}", info.capabilities.compile_wasm);
    println!("    - Compile WebApp: {}", info.capabilities.compile_webapp);
    println!("    - Live Reload: {}", info.capabilities.live_reload);
    println!("    - Optimization: {}", info.capabilities.optimization);
    println!(
        "    - Custom Targets: {:?}",
        info.capabilities.custom_targets
    );
    println!();

    // Test can_handle_project
    println!("Testing project detection:");

    let test_cases = vec![
        ("examples/basic_operations.py", true),
        ("examples/control_flow.py", true),
        ("examples/typed_demo.py", true),
        ("examples/", true),       // Directory with Python files
        ("Cargo.toml", false),     // Non-Python file
        ("nonexistent.py", false), // Non-existent file
    ];

    for (path, expected) in test_cases {
        let result = plugin.can_handle_project(path);
        let status = if result == expected { "✅" } else { "❌" };
        println!("  {status} {path}: {result} (expected: {expected})");
    }
    println!();

    // Test builder
    println!("Testing WasmBuilder:");
    let builder = plugin.get_builder();

    println!("  Language: {}", builder.language_name());
    println!("  Extensions: {:?}", builder.supported_extensions());
    println!("  Entry Candidates: {:?}", builder.entry_file_candidates());
    println!("  Dependencies: {:?}", builder.check_dependencies());
    println!();

    // Test build process with a simple Python file
    if std::path::Path::new("examples/basic_operations.py").exists() {
        println!("Testing build process:");

        let build_config = BuildConfig {
            input: "examples/basic_operations.py".to_string(),
            output_dir: "examples/output/plugin_test".to_string(),
            optimization: OptimizationLevel::Release,
            target_type: "wasm".to_string(),
            verbose: true,
            watch: false,
        };

        match builder.build(&build_config) {
            Ok(result) => {
                println!("  ✅ Build successful!");
                println!("    Output: {}", result.output_path);
                println!("    Language: {}", result.language);
                println!("    Build time: {:?}", result.build_time);
                println!("    File size: {} bytes", result.file_size);

                // Check if file actually exists
                if std::path::Path::new(&result.output_path).exists() {
                    println!("    ✅ Output file verified");
                } else {
                    return Err(format!(
                        "plugin reported {} but wrote nothing there",
                        result.output_path
                    )
                    .into());
                }
            }
            Err(e) => {
                // A failed build used to print a cross and let the process
                // exit 0, so this example could not tell anyone the plugin was
                // broken. `tests/integration/wasmrun_plugin.rs` is the real
                // gate; this at least fails.
                return Err(format!("plugin build failed: {e}").into());
            }
        }
    } else {
        println!("⚠️  Skipping build test (basic_operations.py not found)");
    }

    println!("\n🎉 Plugin test completed!");
    Ok(())
}

#[cfg(not(feature = "wasm-plugin"))]
fn main() {
    println!("❌ Plugin feature not enabled. Run with:");
    println!("   cargo run --example plugin_test --features wasm-plugin");
}
