use anyhow::Result;
use rustpython_parser::ast::Suite;
use rustpython_parser::Parse;

/// Parse Python source code into a RustPython AST.
pub fn parse_python(source: &str) -> Result<Suite> {
    let ast =
        Suite::parse(source, "<module>").map_err(|e| anyhow::anyhow!("Parse error: {:?}", e))?;
    Ok(ast)
}
