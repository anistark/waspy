use crate::ir::IRType;
use std::collections::HashMap;

/// Local variable
pub struct LocalInfo {
    pub index: u32,
    pub var_type: IRType,
}

/// Function
pub struct FunctionInfo {
    pub index: u32,
    // TODO: Add param and return types to function
    #[allow(dead_code)]
    pub param_types: Vec<IRType>,
    pub return_type: IRType,
}

/// Class information for instantiation and method dispatch
pub struct ClassInfo {
    pub name: String,
    pub methods: HashMap<String, u32>, // method_name -> function_index
    pub field_offsets: HashMap<String, u64>, // field_name -> byte_offset
    pub instance_size: u32,            // size of instance in bytes
}

/// Compiled Local variables and function types
pub struct CompilationContext {
    pub locals_map: HashMap<String, LocalInfo>,
    pub local_count: u32,
    pub function_map: HashMap<String, FunctionInfo>,
    pub class_map: HashMap<String, ClassInfo>,
    pub temp_local: u32, // For temporary calculations
}

impl CompilationContext {
    /// Create a new compilation context
    pub fn new() -> Self {
        let mut ctx = CompilationContext {
            locals_map: HashMap::new(),
            local_count: 0,
            function_map: HashMap::new(),
            class_map: HashMap::new(),
            temp_local: 0,
        };

        // Add temporary locals for calculations
        ctx.temp_local = ctx.local_count;
        ctx.local_count += 3;

        ctx
    }

    /// Add a local variable to the context
    pub fn add_local(&mut self, name: &str, var_type: IRType) -> u32 {
        let idx = self.local_count;
        self.locals_map.insert(
            name.to_string(),
            LocalInfo {
                index: idx,
                var_type,
            },
        );
        self.local_count += 1;
        idx
    }

    /// Get information about a local variable by name
    pub fn get_local_info(&self, name: &str) -> Option<&LocalInfo> {
        self.locals_map.get(name)
    }

    /// Get just the index of a local variable by name
    pub fn get_local_index(&self, name: &str) -> Option<u32> {
        self.locals_map.get(name).map(|info| info.index)
    }

    /// Add a function to the context
    pub fn add_function(
        &mut self,
        name: &str,
        index: u32,
        param_types: Vec<IRType>,
        return_type: IRType,
    ) {
        self.function_map.insert(
            name.to_string(),
            FunctionInfo {
                index,
                param_types,
                return_type,
            },
        );
    }

    /// Get information about a function by name
    pub fn get_function_info(&self, name: &str) -> Option<&FunctionInfo> {
        self.function_map.get(name)
    }

    /// Add a class to the context
    pub fn add_class(&mut self, class_info: ClassInfo) {
        self.class_map.insert(class_info.name.clone(), class_info);
    }

    /// Get information about a class by name
    pub fn get_class_info(&self, name: &str) -> Option<&ClassInfo> {
        self.class_map.get(name)
    }
}
