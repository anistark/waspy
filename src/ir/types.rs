/// Intermediate Representation (IR) for a module containing multiple functions
#[derive(Debug)]
pub struct IRModule {
    pub functions: Vec<IRFunction>,
    pub variables: Vec<IRVariable>, // Module-level variables
    pub imports: Vec<IRImport>,     // Module-level imports
    pub classes: Vec<IRClass>,      // Module-level classes
    pub metadata: std::collections::HashMap<String, String>, // Module metadata
    pub memory_layout: MemoryLayout, // String/bytes offsets and object heap layout
}

/// IR representation of a function
#[derive(Debug, Clone)]
pub struct IRFunction {
    pub name: String,
    pub params: Vec<IRParam>,
    pub body: IRBody,
    pub return_type: IRType,
    pub decorators: Vec<String>, // Function decorators
}

/// IR representation of a function parameter
#[derive(Debug, Clone)]
pub struct IRParam {
    pub name: String,
    pub param_type: IRType,
    pub default_value: Option<IRExpr>, // Default parameter values
}

/// IR representation of a function body, which can contain multiple statements
#[derive(Debug, Clone)]
pub struct IRBody {
    pub statements: Vec<IRStatement>,
}

/// IR representation of statements
#[derive(Debug, Clone)]
pub enum IRStatement {
    Return(Option<IRExpr>),
    Assign {
        target: String,
        value: IRExpr,
        var_type: Option<IRType>,
    },
    If {
        condition: IRExpr,
        then_body: Box<IRBody>,
        else_body: Option<Box<IRBody>>,
    },
    Raise {
        exception: Option<IRExpr>,
    },
    While {
        condition: IRExpr,
        body: Box<IRBody>,
    },
    Expression(IRExpr),
    TryExcept {
        try_body: Box<IRBody>,
        except_handlers: Vec<IRExceptHandler>,
        finally_body: Option<Box<IRBody>>,
    },
    For {
        target: String,
        iterable: IRExpr,
        body: Box<IRBody>,
        else_body: Option<Box<IRBody>>,
    },
    With {
        context_expr: IRExpr,
        optional_vars: Option<String>,
        body: Box<IRBody>,
    },
    // New variants for object-oriented programming
    AttributeAssign {
        object: IRExpr,
        attribute: String,
        value: IRExpr,
    },
    AugAssign {
        target: String,
        value: IRExpr,
        op: IROp,
    },
    AttributeAugAssign {
        object: IRExpr,
        attribute: String,
        value: IRExpr,
        op: IROp,
    },
    // New statement for dynamic imports
    DynamicImport {
        target: String,
        module_name: IRExpr,
    },
    // Index assignment like list[index] = value or dict[key] = value
    IndexAssign {
        container: IRExpr,
        index: IRExpr,
        value: IRExpr,
    },
    // Tuple unpacking like a, b = (1, 2). With `starred` set (extended
    // unpacking, `a, *b, c = xs`), that target position collects the middle
    // elements as a fresh list while the others bind positionally from the
    // front and back.
    TupleUnpack {
        targets: Vec<String>,
        value: IRExpr,
        starred: Option<usize>,
    },
    // Yield statement for generators: yield value
    Yield {
        value: Option<IRExpr>,
    },
    // Import module statement for module loading: import module_name
    ImportModule {
        module_name: String,
        alias: Option<String>,
    },
    // Loop control: `break` exits the innermost loop.
    Break,
    // Loop control: `continue` jumps to the next iteration of the innermost loop.
    Continue,
}

/// Except handler for try-except statements
#[derive(Debug, Clone)]
pub struct IRExceptHandler {
    pub exception_type: Option<String>,
    pub name: Option<String>,
    pub body: IRBody,
}

/// Module-level variable
#[derive(Debug)]
pub struct IRVariable {
    pub name: String,
    pub value: IRExpr,
    pub var_type: Option<IRType>,
}

/// Module-level import
#[derive(Clone, Debug)]
pub struct IRImport {
    pub module: String,
    pub name: Option<String>,
    pub alias: Option<String>,
    pub is_from_import: bool,
    // New fields for enhanced import support
    pub is_star_import: bool,               // from module import *
    pub is_conditional: bool,               // in try-except block
    pub is_dynamic: bool,                   // using importlib or __import__
    pub conditional_fallbacks: Vec<String>, // Alternative imports in except blocks
}

/// Class definition
#[derive(Debug)]
pub struct IRClass {
    pub name: String,
    pub bases: Vec<String>,
    pub methods: Vec<IRFunction>,
    pub class_vars: Vec<IRVariable>,
}

/// Expression types in the intermediate representation
#[derive(Debug, Clone)]
pub enum IRExpr {
    Const(IRConstant),
    BinaryOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IROp,
    },
    UnaryOp {
        operand: Box<IRExpr>,
        op: IRUnaryOp,
    },
    CompareOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IRCompareOp,
    },
    Param(String),
    Variable(String),
    FunctionCall {
        function_name: String,
        arguments: Vec<IRExpr>,
    },
    BoolOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IRBoolOp,
    },
    ListLiteral(Vec<IRExpr>),
    DictLiteral(Vec<(IRExpr, IRExpr)>),
    SetLiteral(Vec<IRExpr>),
    TupleLiteral(Vec<IRExpr>),
    Indexing {
        // list[index] or dict[key]
        container: Box<IRExpr>,
        index: Box<IRExpr>,
    },
    Slicing {
        // str[start:end:step] or list[start:end:step]
        container: Box<IRExpr>,
        start: Option<Box<IRExpr>>,
        end: Option<Box<IRExpr>>,
        step: Option<Box<IRExpr>>,
    },
    Attribute {
        // object.attribute
        object: Box<IRExpr>,
        attribute: String,
    },
    // List/set/dict comprehension with one or more generators and optional
    // per-generator filters. `element` is the produced element (the key for a
    // dict comprehension, whose value goes in `value`). Generator targets are
    // pre-renamed to unique names by the converter so a comprehension variable
    // never clobbers a same-named function local (Python 3 scoping).
    Comprehension {
        kind: IRComprehensionKind,
        element: Box<IRExpr>,
        value: Option<Box<IRExpr>>,
        generators: Vec<IRGenerator>,
    },
    MethodCall {
        // object.method(args)
        object: Box<IRExpr>,
        method_name: String,
        arguments: Vec<IRExpr>,
    },
    // New expression for dynamic imports
    DynamicImportExpr {
        // __import__(module_name) or importlib.import_module(module_name)
        module_name: Box<IRExpr>,
    },
    RangeCall {
        // range(start, stop, step)
        start: Option<Box<IRExpr>>,
        stop: Box<IRExpr>,
        step: Option<Box<IRExpr>>,
    },
    Lambda {
        // lambda x: x + 1
        params: Vec<IRParam>,
        body: Box<IRExpr>,
        captured_vars: Vec<String>, // Variables captured from outer scope
    },
    // A lifted lambda (closure creation). The finalize pass replaces every
    // `Lambda` with this after hoisting its body into a real module function
    // named `lambda_name` (whose trailing `__env` parameter carries the
    // environment). Evaluates to a pointer to a fresh heap environment:
    // `[table_slot:i32][captured0:8B][captured1:8B]...` — the table slot at
    // offset 0 drives `call_indirect` dispatch, and each captured enclosing
    // local is copied (by value) into a slot at creation time.
    ClosureMake {
        lambda_name: String,
        captured: Vec<String>,
    },
}

/// Which collection a comprehension builds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IRComprehensionKind {
    List,
    Set,
    Dict,
}

/// One `for target(s) in iterable [if cond]*` clause of a comprehension.
/// Multiple `targets` mean the iterated element is a tuple unpacked
/// positionally (`for k, v in items`).
#[derive(Debug, Clone)]
pub struct IRGenerator {
    pub targets: Vec<String>,
    pub iterable: IRExpr,
    pub conditions: Vec<IRExpr>,
}

/// Constant value types supported in the IR
#[derive(Debug, Clone)]
pub enum IRConstant {
    Int(i32),
    Float(f64),
    Bool(bool),
    String(String),
    None,
    // New constants
    List(Vec<IRConstant>),
    Dict(Vec<(IRConstant, IRConstant)>),
    Tuple(Vec<IRConstant>),
    Bytes(Vec<u8>),
    Set(Vec<IRConstant>),
}

/// Type system for IR
#[derive(Debug, Clone, PartialEq)]
pub enum IRType {
    Int,
    Float,
    Bool,
    String,
    List(Box<IRType>),
    Dict(Box<IRType>, Box<IRType>),
    Any,
    None,
    Unknown,
    // New types
    Tuple(Vec<IRType>),
    Optional(Box<IRType>),
    Union(Vec<IRType>),
    Class(String),
    // New type for modules
    Module(String),
    Bytes,
    Set(Box<IRType>),
    Range,
    Callable {
        params: Vec<IRType>,
        return_type: Box<IRType>,
    },
    Generator(Box<IRType>), // Generator yields values of this type
    // A file object returned by `open()`. At runtime the value is the host
    // file descriptor (a single i32); read/write/close lower to calls into
    // the `waspy_host` import namespace.
    File,
    // Datetime types for proper arithmetic support
    Datetime,  // (year, month, day, hour, minute, second, microsecond)
    Date,      // (year, month, day)
    Time,      // (hour, minute, second, microsecond)
    Timedelta, // (days, seconds, microseconds)
}

/// Binary operators in the IR
#[derive(Debug, Clone, PartialEq)]
pub enum IROp {
    Add,      // +
    Sub,      // -
    Mul,      // *
    Div,      // /
    Mod,      // %
    FloorDiv, // //
    Pow,      // **
    // New operators
    MatMul, // @
    LShift, // <<
    RShift, // >>
    BitOr,  // |
    BitXor, // ^
    BitAnd, // &
}

/// Unary operators in the IR
#[derive(Debug, Clone)]
pub enum IRUnaryOp {
    Neg, // -x
    Not, // not x
    // New unary operators
    Invert, // ~x
    UAdd,   // +x
}

/// Comparison operators in the IR
#[derive(Debug, Clone)]
pub enum IRCompareOp {
    Eq,    // ==
    NotEq, // !=
    Lt,    // <
    LtE,   // <=
    Gt,    // >
    GtE,   // >=
    // New comparison operators
    In,    // in
    NotIn, // not in
    Is,    // is
    IsNot, // is not
}

/// Boolean operators in the IR
#[derive(Debug, Clone)]
pub enum IRBoolOp {
    And, // and
    Or,  // or
}

/// Number of bytes reserved before each interned string/bytes blob to hold its
/// length (an i32). A blob's recorded offset points just past this prefix, so
/// `load(offset - STRING_LEN_PREFIX)` recovers its length even when only the
/// offset word survives (e.g. read back out of a collection slot). Runtime
/// strings built by `__alloc` use the same layout.
pub const STRING_LEN_PREFIX: u32 = 4;

/// Memory layout information for string and object storage
#[derive(Debug, Clone)]
pub struct MemoryLayout {
    pub string_offsets: std::collections::HashMap<String, u32>,
    pub next_string_offset: u32,
    pub bytes_offsets: std::collections::HashMap<Vec<u8>, u32>,
    pub next_bytes_offset: u32,
    pub set_id_counter: u32,
    pub object_heap_offset: u32, // Where object instances are stored
    pub next_object_id: u32,     // Counter for allocating object instances
    /// Counter used while lowering to give each comprehension's loop variables
    /// unique names (`__comp{n}_{orig}`), so they never collide with (or leak
    /// into) same-named function locals — Python 3 comprehension scoping.
    pub comp_var_counter: u32,
}

impl Default for MemoryLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryLayout {
    pub fn new() -> Self {
        // Start objects at 64KB to avoid collision with small offsets
        MemoryLayout {
            string_offsets: std::collections::HashMap::new(),
            next_string_offset: 0,
            bytes_offsets: std::collections::HashMap::new(),
            next_bytes_offset: 32768,
            set_id_counter: 0,
            object_heap_offset: 65536,
            next_object_id: 0,
            comp_var_counter: 0,
        }
    }

    /// Add a string to memory and return its offset
    pub fn add_string(&mut self, s: &str) -> u32 {
        if let Some(&offset) = self.string_offsets.get(s) {
            return offset;
        }

        // Each blob is laid out as [len:i32][bytes...][nul]; the returned offset
        // points at the bytes (past the length prefix), so the length is
        // recoverable as load(offset - STRING_LEN_PREFIX).
        let offset = self.next_string_offset + STRING_LEN_PREFIX;
        self.string_offsets.insert(s.to_string(), offset);

        // Advance past the prefix, the bytes, and the null terminator.
        self.next_string_offset += STRING_LEN_PREFIX + (s.len() + 1) as u32;

        offset
    }

    /// Add bytes to memory and return its offset
    pub fn add_bytes(&mut self, b: &[u8]) -> u32 {
        if let Some(&offset) = self.bytes_offsets.get(b) {
            return offset;
        }

        // Same [len:i32][bytes...] layout as strings (no null terminator); the
        // returned offset points at the bytes, past the length prefix.
        let offset = self.next_bytes_offset + STRING_LEN_PREFIX;
        self.bytes_offsets.insert(b.to_vec(), offset);

        // Advance past the prefix and the bytes.
        self.next_bytes_offset += STRING_LEN_PREFIX + b.len() as u32;

        offset
    }

    /// Merge the string and bytes entries from another layout, assigning fresh
    /// non-colliding offsets here. Used when combining several lowered modules
    /// into one binary. The IR references string/bytes *values* (offsets are
    /// resolved at compile time), so re-adding by value is sufficient; entries
    /// are processed in offset order for deterministic output.
    pub fn merge_from(&mut self, other: &MemoryLayout) {
        let mut strings: Vec<(&String, u32)> =
            other.string_offsets.iter().map(|(s, &o)| (s, o)).collect();
        strings.sort_by_key(|&(_, offset)| offset);
        for (s, _) in strings {
            self.add_string(s);
        }

        let mut bytes: Vec<(&Vec<u8>, u32)> =
            other.bytes_offsets.iter().map(|(b, &o)| (b, o)).collect();
        bytes.sort_by_key(|&(_, offset)| offset);
        for (b, _) in bytes {
            self.add_bytes(b);
        }
    }

    /// Allocate space for an object instance, returns pointer to allocated memory
    pub fn allocate_object(&mut self, size: u32) -> u32 {
        let ptr = self.object_heap_offset;
        self.object_heap_offset += size;
        self.next_object_id += 1;
        ptr
    }

    /// Allocate space for a list, returns pointer to allocated memory
    pub fn allocate_list(&mut self, element_count: u32) -> u32 {
        let size = 4 + (element_count * 4);
        let ptr = self.object_heap_offset;
        self.object_heap_offset += size;
        ptr
    }
}

impl Default for IRModule {
    fn default() -> Self {
        Self::new()
    }
}

impl IRModule {
    /// Create a new empty module
    pub fn new() -> Self {
        IRModule {
            functions: Vec::new(),
            variables: Vec::new(),
            imports: Vec::new(),
            classes: Vec::new(),
            metadata: std::collections::HashMap::new(),
            memory_layout: MemoryLayout::new(),
        }
    }
}
