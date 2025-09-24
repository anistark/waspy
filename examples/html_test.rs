//! Test the HTML generation functionality
//!
//! This example demonstrates the HTML test harness generation feature of the waspy plugin.

#[cfg(feature = "wasm-plugin")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use waspy::wasmrun::{WaspyPlugin, OptimizationLevel, BuildConfig};
    use waspy::wasmrun::Plugin;

    println!("🌐 Testing Waspy HTML Generation");
    println!("=================================\n");

    let plugin = WaspyPlugin::new();
    let builder = plugin.get_builder();

    // Test HTML generation
    if std::path::Path::new("examples/basic_operations.py").exists() {
        println!("Compiling Python with HTML test harness...");

        let build_config = BuildConfig {
            input: "examples/basic_operations.py".to_string(),
            output_dir: "examples/output/html_test".to_string(),
            optimization: OptimizationLevel::Release,
            target_type: "html".to_string(),
            verbose: true,
            watch: false,
        };

        match builder.build(&build_config) {
            Ok(result) => {
                println!("✅ HTML generation successful!");
                println!("  WASM file: {}", result.output_path);
                println!("  Build time: {:?}", result.build_time);
                println!("  File size: {} bytes", result.file_size);

                // Check for HTML file
                let html_path = std::path::Path::new(&result.output_path).with_extension("html");
                if html_path.exists() {
                    println!("  ✅ HTML file generated: {}", html_path.display());

                    // Show first few lines of HTML
                    if let Ok(html_content) = std::fs::read_to_string(&html_path) {
                        println!("\nHTML Preview (first 300 chars):");
                        println!("  {}", &html_content[..html_content.len().min(300)]);
                        if html_content.len() > 300 {
                            println!("  ... (truncated)");
                        }
                    }
                } else {
                    println!("  ❌ HTML file not found");
                }

                println!("\n🎯 Ready to serve!");
                println!("   Open {} in a web browser", html_path.display());
                println!("   Or serve with: python -m http.server 8000");
            }
            Err(e) => {
                println!("❌ HTML generation failed: {}", e);
            }
        }
    } else {
        println!("⚠️  basic_operations.py not found - cannot test HTML generation");
    }

    Ok(())
}

#[cfg(not(feature = "wasm-plugin"))]
fn main() {
    println!("❌ Plugin feature not enabled. Run with:");
    println!("   cargo run --example html_test --features wasm-plugin");
}
