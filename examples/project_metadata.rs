use chakrapy::get_python_project_metadata;
use std::path::Path;

/// Example of extracting metadata from a Python project
///
/// This example demonstrates how to use the project metadata extraction
/// feature to analyze a Python project without compiling it.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_path = Path::new("examples/calculator_project");

    println!("ChakraPy Project Metadata Example");
    println!("Project directory: {}", project_path.display());

    // Get metadata about the project
    println!("Analyzing project...");
    let project_metadata = get_python_project_metadata(project_path)?;

    // Display the results
    if project_metadata.is_empty() {
        println!("No Python modules found in the project.");
    } else {
        println!("\nModules and Functions:");
        
        for (module_name, signatures) in project_metadata {
            println!("\n📦 {}", module_name);
            
            if signatures.is_empty() {
                println!("  No functions defined in this module.");
            } else {
                for (i, sig) in signatures.iter().enumerate() {
                    println!(
                        "  {}. def {}({}) -> {}",
                        i + 1,
                        sig.name,
                        sig.parameters.join(", "),
                        sig.return_type
                    );
                }
            }
        }
    }

    println!("\nAnalysis complete!");
    Ok(())
}
