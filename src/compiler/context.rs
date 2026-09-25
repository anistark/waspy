use crate::core::errors::{unsupported_feature, ChakraError};
use crate::ir::{IRExpr, IRType, MethodKind};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// WASM global holding the pending exception's type code, 0 when none.
pub const EXC_TYPE_GLOBAL: u32 = 2;

/// WASM global counting user-function calls on the stack below the current
/// frame, so a propagating exception can tell whether it is about to escape
/// the program (and must trap) or is returning into a caller that will check.
pub const CALL_DEPTH_GLOBAL: u32 = 3;

/// Number of scratch/temporary locals reserved for intermediate calculations
/// (pointer/index bookkeeping, search loops, type coercion, ...). Reserved
/// per-function in `compile_function` after params and named locals, so these
/// indices never alias real variables. Keep this >= the largest `temp_local + N`
/// offset emitted anywhere in the compiler.
pub const SCRATCH_LOCALS: u32 = 48;

/// Locals reserved per function for a value that has to survive the emission of
/// nested expressions: the receiver of a virtual call while its arguments are
/// emitted, and the block a collection literal is being built into while its
/// elements are. Nested emission is arbitrary codegen over the scratch run, so
/// such a value cannot live there; and a nested user (a literal inside a
/// literal, a virtual call in an argument) must not reuse the outer one's slot,
/// hence one per nesting level. Being locals, they are private to each
/// activation, which is what keeps a recursive call from overwriting them.
/// Nesting deeper than this is a compile error, not a wrong answer.
pub const HELD_LOCALS: u32 = 64;

/// The f64 counterpart of [`HELD_LOCALS`], for a float that must survive the
/// emission of nested expressions (the value an `in` test searches for).
pub const HELD_F64_LOCALS: u32 = 16;

/// The synthesized function that evaluates module-level definitions once, in
/// source order, into their globals. See `compile_ir_module`.
pub const MODULE_INIT_FN: &str = "__module_init";

/// Index of the first WASM global holding a module-level definition; the
/// globals below it are the runtime's own (allocator, StopIteration, the
/// pending exception, call depth).
pub const FIRST_MODULE_GLOBAL: u32 = 4;

/// Where the runtime heap starts: every collection, instance, and runtime
/// string is an `__alloc` block from here up. Sits above the string (from 0)
/// and bytes (from 32768) regions so nothing allocated overlaps them. (The
/// 65536..131072 range was once a fixed object-instance region and later held
/// compile-time collection templates; both are gone, so it is unused space.)
pub const COLLECTION_HEAP_BASE: u32 = 131072;

/// Bytes reserved at the start of every collection region for its header: the
/// element/entry count (an `i32` at offset 0), the capacity in elements/entries
/// (an `i32` at [`COLLECTION_CAP`]), and a pointer to the block holding the
/// elements themselves (an `i32` at [`COLLECTION_DATA`]), plus a pad word that
/// keeps the block that follows 8-byte aligned.
///
/// The elements live *behind a pointer* so that growing a collection does not
/// move the collection: `append` reallocates the data block and stores the new
/// pointer in the header, and every alias, a caller's variable, an element of
/// another collection, a field, keeps seeing the same header and therefore the
/// grown contents. A freshly built collection puts its data block immediately
/// after the header, so construction still writes at compile-time addresses.
pub const COLLECTION_HEADER: u32 = 16;

/// Offset of the capacity word inside a collection header. A region whose
/// capacity reads as 0 (or below its own length) is treated as full, so a
/// collection built without a capacity is copied on the next append rather than
/// trusted, the conservative direction.
pub const COLLECTION_CAP: u32 = 4;

/// Offset of the data-block pointer inside a collection header. Element `i`
/// lives at `load(region + COLLECTION_DATA) + i*COLLECTION_SLOT`, never at a
/// fixed offset from the region itself.
pub const COLLECTION_DATA: u32 = 8;

/// Bytes per collection element slot. Wide enough to hold an `f64` without loss,
/// so float elements round-trip exactly; narrower values (i32 ints/bools,
/// interned string/bytes offsets, collection pointers) occupy the low 4 bytes
/// and ignore the high 4. List/tuple/set element `i` lives at
/// `data + i*COLLECTION_SLOT`, where `data` is the region's
/// [`COLLECTION_DATA`] pointer; a dict entry is two consecutive slots (key then
/// value), so entry `i` starts at `data + i*2*COLLECTION_SLOT`. Slots sit at 4-byte (not 8-byte)
/// alignment, which WASM permits: alignment in a load/store is only an
/// optimization hint and never affects correctness.
pub const COLLECTION_SLOT: u32 = 8;

/// Byte stride between consecutive dict entries (a key slot plus a value slot).
pub const DICT_ENTRY: u32 = COLLECTION_SLOT * 2;

/// Name of the companion local that holds the length of a string/bytes local.
/// A string/bytes value is an `(offset, length)` pair but a WASM local holds a
/// single word, so the offset lives in the named local and the length in this
/// companion. See the string/bytes handling in assignment and variable reads.
pub fn strlen_local_name(name: &str) -> String {
    format!("__strlen_{name}")
}

/// Name of a per-comprehension helper local, indexed by comprehension nesting
/// depth (`res` result pointer, `widx` write index, `cap` capacity, `elem`
/// tuple-unpack scratch, and for set comprehensions `mask`/`hidx`/`bkt` hash
/// probing state). Reserved during the local-allocation scan; sibling
/// comprehensions at the same depth share them safely because their
/// evaluations never overlap.
pub fn comp_local_name(role: &str, depth: u32) -> String {
    format!("__comp_{role}_{depth}")
}

/// Name of a per-generator helper local of a comprehension (`ptr`/`idx`/`len`
/// iterator state), indexed by comprehension depth and generator position.
pub fn comp_gen_local_name(role: &str, depth: u32, gen: usize) -> String {
    format!("__comp_{role}_{depth}_{gen}")
}

/// Local variable
pub struct LocalInfo {
    pub index: u32,
    pub var_type: IRType,
}

/// Branch targets for an enclosing loop, used to lower `break`/`continue`.
///
/// Each field records the value of [`CompilationContext::block_depth`] captured
/// immediately after the target frame was opened. A branch emitted at the
/// current depth `c` reaches that frame with `Br(c - level)`, so `break` targets
/// the loop's outer block and `continue` targets an inner block wrapped around
/// the loop body (whose end falls through to the iterator step).
#[derive(Clone, Copy)]
pub struct LoopContext {
    pub break_level: u32,
    pub continue_level: u32,
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
    /// Immediate base class, if any (`object` doesn't count). A subclass lays
    /// its base's fields out as a prefix (same offsets) and appends its own, so
    /// a base method reading `self.x` works unchanged on a subclass instance.
    pub base: Option<String>,
    /// Small integer identifying this class at runtime. Stamped into the tag
    /// word at offset 0 of every instance by `__alloc_obj`, and compared by
    /// `isinstance`. Ids start at 1 so zeroed memory never matches a class.
    pub class_id: i32,
    pub methods: HashMap<String, u32>, // method_name -> function_index
    /// Which class textually defines each method. For an inherited method this
    /// names the base, so the `Owner::method` qualified lookup (parameter and
    /// return types) resolves even though `Sub::method` was never registered.
    pub method_owner: HashMap<String, String>,
    /// How each method binds its first argument (`@staticmethod`,
    /// `@classmethod`, `@property`, or a plain instance method). Call sites and
    /// attribute reads dispatch on this. Property setters are absent here —
    /// they live in `property_setters`, keyed by the same name as the getter.
    pub method_kinds: HashMap<String, MethodKind>,
    /// Property setters: property name -> (function index, defining class).
    /// The defining class resolves the `Owner::name::setter` qualified lookup
    /// for the setter's parameter types.
    pub property_setters: HashMap<String, (u32, String)>,
    pub field_offsets: HashMap<String, u64>, // field_name -> byte_offset
    pub field_types: HashMap<String, IRType>, // field_name -> value type (f64 vs i32)
    pub class_var_values: HashMap<String, IRExpr>, // class-level var name -> initializer
    pub instance_size: u32,                  // size of instance in bytes
}

/// WASM function indices of the `waspy_host` file-I/O imports. Present only
/// when the module actually uses file operations — an import section is
/// emitted solely in that case, so modules without file I/O keep instantiating
/// with an empty import object.
#[derive(Clone, Copy)]
pub struct FileIoImports {
    /// `waspy_host.open(path_ptr, path_len, flags) -> fd` (-1 on failure).
    pub open: u32,
    /// `waspy_host.read(fd, buf_ptr, len) -> bytes_read` (0 = EOF, <0 error).
    pub read: u32,
    /// `waspy_host.write(fd, buf_ptr, len) -> bytes_written` (<0 error).
    pub write: u32,
    /// `waspy_host.close(fd) -> status` (0 = ok).
    pub close: u32,
}

/// Compiled Local variables and function types
pub struct CompilationContext {
    /// Each lifted lambda's parameter name and returned expression, keyed by
    /// the synthesized `__lambda_N` name.
    ///
    /// The finalize pass replaces every `Lambda` with a `ClosureMake` before
    /// code generation runs, so a call site can no longer see what the lambda
    /// computes. `sorted(seq, key=...)` needs exactly that to know what type
    /// the key produces, and therefore how to compare two of them.
    pub lambda_bodies: HashMap<String, (String, crate::ir::IRExpr)>,
    pub locals_map: HashMap<String, LocalInfo>,
    pub local_count: u32,
    pub function_map: HashMap<String, FunctionInfo>,
    pub class_map: HashMap<String, ClassInfo>,
    /// Imported user-written modules: binding name in this namespace (the
    /// module name, or its `import mod as m` alias) -> real module name. A
    /// user module's functions/classes/constants are statically linked into
    /// this single WASM module, so `mod.f(...)` resolves to the merged `f`.
    pub user_modules: HashMap<String, String>,
    /// `from mod import name as alias` bindings for user modules:
    /// alias -> real (merged) function or class name.
    pub import_aliases: HashMap<String, String>,
    /// Virtual method dispatch: the column each overridden method name takes in
    /// the vtable, and the `call_indirect` type index its implementations share.
    /// A method name is here only when some class overrides an ancestor's
    /// definition of it, so a program with no overrides emits no table and
    /// every call stays direct.
    pub virtual_slots: HashMap<String, (u32, u32)>,
    /// Table index of vtable row 0. The rows share table 0 with the closure
    /// dispatch slots, which occupy the indices below this.
    pub vtable_base: u32,
    /// Columns per row, so row `c` starts at `vtable_base + c * vtable_stride`.
    pub vtable_stride: u32,
    /// The vtable's own entries (function indices), row-major, so a call site
    /// can ask what a column can reach without the table section.
    pub vtable_entries: Vec<u32>,
    /// Locals an item assignment (`c[k] = v`) holds its container and key in
    /// while the key and the value are emitted. See `IRStatement::IndexAssign`.
    pub assign_container_local: u32,
    pub assign_key_local: u32,
    pub assign_key_f64_local: u32,
    /// Base index of this function's run of [`HELD_LOCALS`] slots.
    pub held_local_base: u32,
    /// How many held slots are in use, which picks the next one. A `Cell`
    /// because expression codegen holds `&CompilationContext`.
    pub held_depth: Cell<u32>,
    /// Indices of this function's [`HELD_F64_LOCALS`] f64 held slots, and how
    /// many are in use.
    pub held_f64_locals: Vec<u32>,
    pub held_f64_depth: Cell<u32>,
    /// `@functools.singledispatch` tables, keyed by the base function's name:
    /// (registered type, implementation function name) in registration
    /// order. Call codegen picks the arm matching the first argument's static
    /// type and falls back to the base function itself.
    pub dispatch_tables: HashMap<String, Vec<(IRType, String)>>,
    /// File-I/O host import indices; `Some` only when the module uses file
    /// operations (an `open()` call somewhere in its IR).
    pub file_io: Option<FileIoImports>,
    /// Module-level definitions that live in a WASM global rather than being
    /// inlined at every read: name -> global index. Every definition that is
    /// not a plain constant is one, because inlining it made each read a new
    /// object (`G.append(2); len(G)` answered 1) and re-ran its initializer.
    pub module_global_index: HashMap<String, u32>,
    /// The type each module global's initializer produced, recorded while
    /// `__module_init` is compiled, which happens before any other function.
    pub module_global_types: HashMap<String, IRType>,
    /// True while `__module_init` is being compiled: an assignment to a module
    /// global there stores the global instead of creating a local.
    pub in_module_init: bool,
    /// Module-level variables, by name -> (declared type, initializer). Read
    /// references to these are inlined by emitting the initializer expression.
    pub module_vars: HashMap<String, (Option<IRType>, IRExpr)>,
    pub temp_local: u32,     // For temporary calculations (i32 scratch)
    pub temp_local_f64: u32, // Single f64 scratch local (operand juggling for coercions)
    /// Second f64 scratch local. Needed where two f64 values must be live at
    /// once, e.g. indexing a float-keyed *and* float-valued dict: the key needle
    /// sits here while the looked-up value uses `temp_local_f64`.
    pub temp_local_f64_2: u32,
    /// Third f64 scratch local. The float power helper needs base, exponent,
    /// and an accumulating result live at once.
    pub temp_local_f64_3: u32,
    /// Sequence counter for `for` loops, advanced in identical pre-order by the
    /// local-allocation scan and by codegen so each loop reuses the iterator
    /// helper locals (`__iter_*_{n}`) reserved for it. Reset per function.
    pub for_loop_seq: u32,
    /// Number of WASM structured-control frames (`block`/`loop`/`if`) currently
    /// open at the codegen point. Bumped around the body of each construct that
    /// can wrap a `break`/`continue` and unwound on the matching `end`. Combined
    /// with `loop_stack` it yields the relative branch depth for loop control.
    /// Reset per function.
    pub block_depth: u32,
    /// Stack of enclosing loops (innermost last). Empty outside any loop; a
    /// `break`/`continue` with an empty stack is a compile error (Python would
    /// raise `SyntaxError`). Reset per function.
    pub loop_stack: Vec<LoopContext>,
    /// Comprehension nesting depth at the current codegen point. A
    /// comprehension's helper locals (`__comp_*_{d}` / `__comp_*_{d}_{g}`)
    /// are indexed by this depth, reserved during the local-allocation scan;
    /// sibling comprehensions at the same depth safely share them because
    /// their evaluations never overlap in time. Also consulted (like
    /// `loop_stack`) to decide whether a collection literal must be copied out
    /// of its compile-time template region: inside a comprehension loop the
    /// template is rebuilt per iteration. A `Cell` because expression codegen
    /// holds `&CompilationContext`. Reset per function.
    pub comp_depth: Cell<u32>,
    /// WASM function index of the runtime bump allocator `__alloc(size) -> ptr`.
    /// Emitted after all user functions/methods, so callers (e.g. string/bytes
    /// concatenation) reference it by this index. Set during module assembly.
    pub alloc_func_index: u32,
    /// WASM function index of `__alloc_obj(size, class_id) -> ptr`, which
    /// allocates an instance and stamps its class id into the tag word at
    /// offset 0. A separate helper (rather than inline stamping) keeps the
    /// instantiation sequence stack-only, so nested instantiations compose
    /// without touching any scratch local. Set during module assembly.
    pub alloc_obj_func_index: u32,
    /// WASM function index of `__i32_to_str(value) -> offset`, which renders an
    /// i32 as its decimal digits in a fresh `__alloc` blob (`[len][digits][nul]`,
    /// offset past the prefix) — the runtime half of `str(int)`. Set during
    /// module assembly.
    pub i32_to_str_func_index: u32,
    /// Funcref-table slot of each lifted lambda function, keyed by its
    /// `__lambda_{n}` name. `ClosureMake` stamps the slot into the closure
    /// environment's first word; `call_indirect` dispatches through it. Set
    /// during module assembly.
    pub lambda_slots: HashMap<String, u32>,
    /// Type-section index of the closure-call signature with zero user
    /// parameters (`(env: i32) -> i32`); arity `a`'s signature sits at
    /// `closure_type_base + a`. `None` when the module has no lambdas (no
    /// table is emitted). Set during module assembly.
    pub closure_type_base: Option<u32>,
    /// Largest user-parameter count across the module's lambdas; a closure
    /// call site with more arguments than this has no matching signature and
    /// falls back to yielding 0.
    pub closure_max_arity: u32,
    /// True while compiling an `__init__` method. Its `return` paths (explicit
    /// bare `return` and the implicit fall-through) yield `self` (local 0)
    /// instead of the usual 0, so `ClassName(...)` receives the instance
    /// pointer directly as the constructor call's result. Set per function in
    /// `compile_function`.
    pub return_self: bool,
    /// Name of the class whose method is currently being compiled, if any.
    /// `super().method(...)` resolves the base class through this.
    pub current_class: Option<String>,
    /// Resolved return type of the function currently being compiled.
    /// `raise StopIteration` returns the matching default value (f64 0.0 vs
    /// i32 0) after setting the stop flag. Set per function in
    /// `compile_function`.
    pub current_return_type: IRType,
    /// Name of the function currently being compiled, used to locate the
    /// errors reported through [`CompilationContext::report`]. Set per
    /// function in `compile_function`.
    pub current_function: Option<String>,
    /// Block depth of each enclosing `try` body's exception block, innermost
    /// last. A `raise` (or a call that returns with an exception pending)
    /// branches to the innermost one, which lands on that `try`'s handler
    /// dispatch; with the stack empty the exception leaves the function
    /// instead. Reset per function, and popped before the handlers are
    /// compiled so an exception raised inside a handler is not caught by its
    /// own `try`.
    pub try_stack: Vec<u32>,
    /// WASM indices of the functions that can raise, directly or through
    /// something they call. Calls to anything else need no exception check, so
    /// a program that never raises compiles to exactly what it did before.
    pub can_raise: std::collections::HashSet<u32>,
    /// Errors found during code generation.
    ///
    /// Codegen runs behind a shared `&CompilationContext` and its emit
    /// functions return the expression's type, not a `Result`, so a construct
    /// it cannot generate code for (a method on a receiver kind with no method
    /// support, above all) used to have no way to say so and could only emit a
    /// trap. Reporting into this sink turns those into real compile errors:
    /// the emitted code still traps, keeping the module valid and the stack
    /// balanced, but `compile_ir_module` fails with the collected errors
    /// instead of handing back a module that dies at runtime.
    pub errors: RefCell<Vec<ChakraError>>,
}

impl CompilationContext {
    /// Create a new compilation context
    pub fn new() -> Self {
        // Scratch locals are reserved per-function in `compile_function` (after
        // params and named locals are allocated), so nothing is reserved here.
        CompilationContext {
            lambda_bodies: HashMap::new(),
            locals_map: HashMap::new(),
            local_count: 0,
            function_map: HashMap::new(),
            class_map: HashMap::new(),
            user_modules: HashMap::new(),
            import_aliases: HashMap::new(),
            virtual_slots: HashMap::new(),
            vtable_base: 0,
            vtable_stride: 0,
            vtable_entries: Vec::new(),
            assign_container_local: 0,
            assign_key_local: 0,
            assign_key_f64_local: 0,
            held_local_base: 0,
            held_depth: Cell::new(0),
            held_f64_locals: Vec::new(),
            held_f64_depth: Cell::new(0),
            dispatch_tables: HashMap::new(),
            file_io: None,
            module_vars: HashMap::new(),
            module_global_index: HashMap::new(),
            module_global_types: HashMap::new(),
            in_module_init: false,
            temp_local: 0,
            temp_local_f64: 0,
            temp_local_f64_2: 0,
            for_loop_seq: 0,
            block_depth: 0,
            loop_stack: Vec::new(),
            comp_depth: Cell::new(0),
            alloc_func_index: 0,
            alloc_obj_func_index: 0,
            i32_to_str_func_index: 0,
            lambda_slots: HashMap::new(),
            closure_type_base: None,
            closure_max_arity: 0,
            return_self: false,
            current_class: None,
            temp_local_f64_3: 0,
            current_function: None,
            try_stack: Vec::new(),
            can_raise: std::collections::HashSet::new(),
            errors: RefCell::new(Vec::new()),
            current_return_type: IRType::Unknown,
        }
    }

    /// Resolve a called or instantiated name through the module's
    /// `from mod import x as y` aliases. A real definition (function or
    /// class) of the name itself always wins over an alias.
    /// Record a code-generation error against the function being compiled.
    ///
    /// The caller still emits code (a trap, and whatever drops keep the stack
    /// balanced) so the module stays valid and the rest of the program is
    /// compiled, surfacing every such error in one run rather than one per
    /// compile. `compile_ir_module` fails once the whole module is walked.
    pub fn report(&self, message: impl Into<String>) {
        // The IR carries no source spans, so the function being compiled is
        // the finest location available; it goes in the message rather than an
        // `ErrorLocation`, which would have to invent a line number.
        let message = match &self.current_function {
            Some(name) => format!("{} (in function '{name}')", message.into()),
            None => message.into(),
        };
        self.errors
            .borrow_mut()
            .push(unsupported_feature(message, None));
    }

    pub fn resolve_import_alias<'a>(&'a self, name: &'a str) -> &'a str {
        if self.function_map.contains_key(name) || self.class_map.contains_key(name) {
            return name;
        }
        self.import_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name)
    }

    /// True if `sub` is `base` or reaches `base` by walking single-inheritance
    /// `base` links. Both must be known classes.
    pub fn is_class_or_subclass(&self, sub: &str, base: &str) -> bool {
        let mut current = Some(sub.to_string());
        while let Some(name) = current {
            if name == base {
                return true;
            }
            current = self.class_map.get(&name).and_then(|info| info.base.clone());
        }
        false
    }

    /// Class ids of `target` and every known subclass of it — the id set an
    /// instance tag may hold when the value `isinstance`-checks against
    /// `target`. Sorted for deterministic codegen.
    pub fn assignable_class_ids(&self, target: &str) -> Vec<i32> {
        let mut ids: Vec<i32> = self
            .class_map
            .values()
            .filter(|info| self.is_class_or_subclass(&info.name, target))
            .map(|info| info.class_id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Does a call to `method_name` on a receiver statically typed `class_name`
    /// have to dispatch through the vtable?
    ///
    /// Only when some class assignable to `class_name` resolves the name to a
    /// different implementation than `class_name` itself does. A method nothing
    /// overrides, a leaf class, and a call on a class with no subclasses all
    /// answer `None` and keep the direct call they have always had, so a
    /// program without overrides compiles to exactly what it did before.
    ///
    /// Returns the vtable column and the `call_indirect` type index.
    pub fn virtual_call(&self, class_name: &str, method_name: &str) -> Option<(u32, u32)> {
        let &(column, type_index) = self.virtual_slots.get(method_name)?;
        let own = self.get_class_info(class_name)?.methods.get(method_name)?;
        let overridden = self
            .class_map
            .values()
            .filter(|info| self.is_class_or_subclass(&info.name, class_name))
            .any(|info| info.methods.get(method_name) != Some(own));
        overridden.then_some((column, type_index))
    }

    /// Every implementation reachable through vtable column `column`: one per
    /// class row, so a caller can ask whether any of them can raise.
    pub fn virtual_implementations(&self, column: u32) -> impl Iterator<Item = u32> + '_ {
        let stride = self.vtable_stride;
        self.vtable_entries
            .iter()
            .skip(column as usize)
            .step_by(stride.max(1) as usize)
            .copied()
    }

    /// Take the next held slot, for a value that must survive nested emission.
    /// `None` past [`HELD_LOCALS`], which the caller reports. Every `Some` must
    /// be paired with [`release_held`](Self::release_held) once the nested
    /// emission is done; the slot's contents stay readable after release, until
    /// something takes it again.
    pub fn hold(&self) -> Option<u32> {
        let depth = self.held_depth.get();
        if depth >= HELD_LOCALS {
            return None;
        }
        self.held_depth.set(depth + 1);
        Some(self.held_local_base + depth)
    }

    /// Give back the slot the matching [`hold`](Self::hold) took.
    pub fn release_held(&self) {
        self.held_depth.set(self.held_depth.get() - 1);
    }

    /// [`hold`](Self::hold) for an f64. Pair with
    /// [`release_held_f64`](Self::release_held_f64).
    pub fn hold_f64(&self) -> Option<u32> {
        let depth = self.held_f64_depth.get();
        let slot = self.held_f64_locals.get(depth as usize).copied()?;
        self.held_f64_depth.set(depth + 1);
        Some(slot)
    }

    pub fn release_held_f64(&self) {
        self.held_f64_depth.set(self.held_f64_depth.get() - 1);
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
