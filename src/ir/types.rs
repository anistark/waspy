/// Intermediate Representation (IR) for a module containing multiple functions
#[derive(Debug)]
pub struct IRModule {
    pub functions: Vec<IRFunction>,
    pub variables: Vec<IRVariable>, // Module-level variables
    pub imports: Vec<IRImport>,     // Module-level imports
    pub classes: Vec<IRClass>,      // Module-level classes
    pub metadata: std::collections::HashMap<String, String>, // Module metadata
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
    // New expressions
    ListComp {
        // [expr for x in iterable]
        expr: Box<IRExpr>,
        var_name: String,
        iterable: Box<IRExpr>,
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

/// Memory layout information for string and object storage
#[derive(Debug, Clone)]
pub struct MemoryLayout {
    pub string_offsets: std::collections::HashMap<String, u32>,
    pub next_string_offset: u32,
    pub bytes_offsets: std::collections::HashMap<Vec<u8>, u32>,
    pub next_bytes_offset: u32,
    pub object_heap_offset: u32, // Where object instances are stored
    pub next_object_id: u32,     // Counter for allocating object instances
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
            object_heap_offset: 65536,
            next_object_id: 0,
        }
    }

    /// Add a string to memory and return its offset
    pub fn add_string(&mut self, s: &str) -> u32 {
        if let Some(&offset) = self.string_offsets.get(s) {
            return offset;
        }

        let offset = self.next_string_offset;
        self.string_offsets.insert(s.to_string(), offset);

        // Advance offset by string length + null terminator
        self.next_string_offset += (s.len() + 1) as u32;

        offset
    }

    /// Add bytes to memory and return its offset
    pub fn add_bytes(&mut self, b: &[u8]) -> u32 {
        if let Some(&offset) = self.bytes_offsets.get(b) {
            return offset;
        }

        let offset = self.next_bytes_offset;
        self.bytes_offsets.insert(b.to_vec(), offset);

        // Advance offset by bytes length
        self.next_bytes_offset += b.len() as u32;

        offset
    }

    /// Allocate space for an object instance, returns pointer to allocated memory
    pub fn allocate_object(&mut self, size: u32) -> u32 {
        let ptr = self.object_heap_offset;
        self.object_heap_offset += size;
        self.next_object_id += 1;
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
        }
    }
}
