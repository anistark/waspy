/// Intermediate Representation (IR) for a module containing multiple functions
pub struct IRModule {
    pub functions: Vec<IRFunction>,
}

/// IR representation of a function
pub struct IRFunction {
    pub name: String,
    pub params: Vec<String>,
    pub body: IRBody,
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
}

/// Constant value types supported in the IR
#[derive(Debug, Clone)]
pub enum IRConstant {
    Int(i32),
    Float(f64),
    Bool(bool),
    String(String),
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
