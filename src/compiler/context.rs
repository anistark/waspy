use std::collections::HashMap;

/// Context for tracking local variables and function types during compilation
pub struct CompilationContext {
    pub locals: HashMap<String, u32>,
    pub local_count: u32,
    pub function_types: HashMap<String, u32>,
}

impl CompilationContext {
    /// Create a new compilation context
    pub fn new() -> Self {
        CompilationContext {
            locals: HashMap::new(),
            local_count: 0,
            function_types: HashMap::new(),
        }
    }

    /// Add a local variable to the context
    pub fn add_local(&mut self, name: &str) -> u32 {
        let idx = self.local_count;
        self.locals.insert(name.to_string(), idx);
        self.local_count += 1;
        idx
    }

    /// Get the index of a local variable by name
    pub fn get_local(&self, name: &str) -> Option<u32> {
        self.locals.get(name).copied()
    }
}
