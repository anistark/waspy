use std::fs;
use std::path::Path;
use waspy::compile_python_to_wasm;

struct TestResult {
    name: String,
    passed: bool,
    error: Option<String>,
}

fn test_file(file_path: &str, test_name: &str) -> TestResult {
    let path = Path::new(file_path);

    if !path.exists() {
        return TestResult {
            name: test_name.to_string(),
            passed: false,
            error: Some(format!("File not found: {}", file_path)),
        };
    }

    let python_code = match fs::read_to_string(path) {
        Ok(code) => code,
        Err(e) => {
            return TestResult {
                name: test_name.to_string(),
                passed: false,
                error: Some(format!("Failed to read file: {}", e)),
            }
        }
    };

    match compile_python_to_wasm(&python_code) {
        Ok(_wasm_bytes) => TestResult {
            name: test_name.to_string(),
            passed: true,
            error: None,
        },
        Err(e) => TestResult {
            name: test_name.to_string(),
            passed: false,
            error: Some(format!("{}", e)),
        },
    }
}

fn main() {
    println!("Testing Standard Library Module Compilation");
    println!("============================================\n");

    let tests = vec![
        ("examples/test_sys.py", "sys module"),
        ("examples/test_os.py", "os module"),
        ("examples/test_re.py", "re module"),
        ("examples/test_datetime.py", "datetime module"),
        ("examples/test_all_stdlib_imports.py", "all stdlib imports"),
    ];

    let mut results = Vec::new();
    let mut passed = 0;
    let mut failed = 0;

    for (file_path, test_name) in tests {
        print!("Testing {}... ", test_name);
        let result = test_file(file_path, test_name);

        if result.passed {
            println!("✅ PASS");
            passed += 1;
        } else {
            println!("❌ FAIL");
            if let Some(error) = &result.error {
                println!("  Error: {}", error);
            }
            failed += 1;
        }

        results.push(result);
    }

    println!("\n============================================");
    println!("Results: {} passed, {} failed\n", passed, failed);

    if failed == 0 {
        println!("✅ All stdlib module tests passed!");
        std::process::exit(0);
    } else {
        println!("❌ Some tests failed");
        println!("\nFailed tests:");
        for result in results.iter().filter(|r| !r.passed) {
            println!("  - {}", result.name);
            if let Some(error) = &result.error {
                println!("    {}", error);
            }
        }
        std::process::exit(1);
    }
}
