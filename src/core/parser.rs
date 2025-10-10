use crate::log_verbose;
use anyhow::Result;
use rustpython_parser::ast::Suite;
use rustpython_parser::Parse;

/// Parse Python source code into AST
pub fn parse_python(source: &str) -> Result<Suite> {
    let ast =
        Suite::parse(source, "<module>").map_err(|e| anyhow::anyhow!("Parse error: {e:?}"))?;

    // Log AST in verbose mode
    if crate::utils::logging::get_verbosity().is_verbose() {
        log_verbose!("Python AST:");
        log_verbose!("{:#?}", ast);
    }

    Ok(ast)
}
