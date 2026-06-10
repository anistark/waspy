use crate::ir::{IRExpr, IRType};
use std::cell::Cell;
use std::collections::HashMap;

/// Number of scratch/temporary locals reserved for intermediate calculations
/// (pointer/index bookkeeping, search loops, type coercion, ...). Reserved
/// per-function in `compile_function` after params and named locals, so these
/// indices never alias real variables. Keep this >= the largest `temp_local + N`
/// offset emitted anywhere in the compiler.
pub const SCRATCH_LOCALS: u32 = 8;

/// Base address of the collection heap. Sits above the string (from 0), bytes
/// (from 32768), and object-instance (from 65536) regions so collection
/// literals never overlap them.
pub const COLLECTION_HEAP_BASE: u32 = 131072;

/// Name of the companion local that holds the length of a string/bytes local.
/// A string/bytes value is an `(offset, length)` pair but a WASM local holds a
/// single word, so the offset lives in the named local and the length in this
/// companion. See the string/bytes handling in assignment and variable reads.
pub fn strlen_local_name(name: &str) -> String {
    format!("__strlen_{name}")
}

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
    pub field_types: HashMap<String, IRType>, // field_name -> value type (f64 vs i32)
    pub class_var_values: HashMap<String, IRExpr>, // class-level var name -> initializer
    pub instance_size: u32,            // size of instance in bytes
}

/// Compiled Local variables and function types
pub struct CompilationContext {
    pub locals_map: HashMap<String, LocalInfo>,
    pub local_count: u32,
    pub function_map: HashMap<String, FunctionInfo>,
    pub class_map: HashMap<String, ClassInfo>,
    /// Module-level variables, by name -> (declared type, initializer). Read
    /// references to these are inlined by emitting the initializer expression.
    pub module_vars: HashMap<String, (Option<IRType>, IRExpr)>,
    pub temp_local: u32,     // For temporary calculations (i32 scratch)
    pub temp_local_f64: u32, // Single f64 scratch local (operand juggling for coercions)
    /// Sequence counter for `for` loops, advanced in identical pre-order by the
    /// local-allocation scan and by codegen so each loop reuses the iterator
    /// helper locals (`__iter_*_{n}`) reserved for it. Reset per function.
    pub for_loop_seq: u32,
    /// Running high-water mark of the collection heap, in bytes past
    /// `COLLECTION_HEAP_BASE`. Each literal reserves a fresh region here, so it
    /// grows monotonically across the whole module. A `Cell` because codegen
    /// holds `&CompilationContext` while allocating.
    pub collection_alloc_offset: Cell<u32>,
}

impl CompilationContext {
    /// Create a new compilation context
    pub fn new() -> Self {
        // Scratch locals are reserved per-function in `compile_function` (after
        // params and named locals are allocated), so nothing is reserved here.
        CompilationContext {
            locals_map: HashMap::new(),
            local_count: 0,
            function_map: HashMap::new(),
            class_map: HashMap::new(),
            module_vars: HashMap::new(),
            temp_local: 0,
            temp_local_f64: 0,
            for_loop_seq: 0,
            collection_alloc_offset: Cell::new(0),
        }
    }

    /// Reserve a fresh, uniquely addressed region of `size` bytes for a
    /// collection literal and return its compile-time base pointer. Distinct
    /// (and nested) literals get distinct regions, so they never alias. Sizes
    /// are rounded up to 8 bytes to keep f64/dict-entry slots aligned.
    pub fn alloc_collection(&self, size: u32) -> u32 {
        let aligned = (size + 7) & !7;
        let offset = self.collection_alloc_offset.get();
        self.collection_alloc_offset.set(offset + aligned);
        COLLECTION_HEAP_BASE + offset
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

    /// Register a module-level variable and its initializer.
    pub fn add_module_var(&mut self, name: &str, var_type: Option<IRType>, value: IRExpr) {
        self.module_vars.insert(name.to_string(), (var_type, value));
    }

    /// Look up a module-level variable's (declared type, initializer).
    pub fn get_module_var(&self, name: &str) -> Option<&(Option<IRType>, IRExpr)> {
        self.module_vars.get(name)
    }
}
