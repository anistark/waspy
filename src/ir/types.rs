/// Intermediate Representation (IR) for a module containing multiple functions
pub struct IRModule {
    pub functions: Vec<IRFunction>,
}

/// IR representation of a function
pub struct IRFunction {
    pub name: String,
    pub params: Vec<IRParam>,
    pub body: IRBody,
    pub return_type: IRType,
}

/// IR representation of a function parameter
pub struct IRParam {
    pub name: String,
    pub param_type: IRType,
}

/// IR representation of a function body, which can contain multiple statements
pub struct IRBody {
    pub statements: Vec<IRStatement>,
}

/// IR representation of statements
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
    While {
        condition: IRExpr,
        body: Box<IRBody>,
    },
    Expression(IRExpr),
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
    Attribute {
        // object.attribute
        object: Box<IRExpr>,
        attribute: String,
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
}

/// Binary operators in the IR
#[derive(Debug, Clone)]
pub enum IROp {
    Add,      // +
    Sub,      // -
    Mul,      // *
    Div,      // /
    Mod,      // %
    FloorDiv, // //
    Pow,      // **
}

/// Unary operators in the IR
#[derive(Debug, Clone)]
pub enum IRUnaryOp {
    Neg, // -x
    Not, // not x
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
}

/// Boolean operators in the IR
#[derive(Debug, Clone)]
pub enum IRBoolOp {
    And, // and
    Or,  // or
}

/// Memory layout information for string storage
#[derive(Debug, Clone)]
pub struct MemoryLayout {
    pub string_offsets: std::collections::HashMap<String, u32>,
    pub next_string_offset: u32,
}

impl Default for MemoryLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryLayout {
    pub fn new() -> Self {
        MemoryLayout {
            string_offsets: std::collections::HashMap::new(),
            next_string_offset: 0,
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
}
