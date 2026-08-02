use crate::error::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceKind {
    Kick,
    Snare,
    Hat,
    Bass,
    Lead,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub midi: u8,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Voice(VoiceKind, Span),
    PatternString(String, Span),
    Notes(Vec<Note>, Span),
    Tuple(Vec<Note>, String, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Combinator {
    Rev,
    Every(u64, Box<Combinator>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BindStmt {
    pub voice: Expr,
    pub pattern: Expr,
    pub combinators: Vec<Combinator>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoopStmt {
    pub name: String,
    pub tempo: Option<f64>,
    pub binds: Vec<BindStmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        value: Expr,
        span: Span,
    },
    Loop(LoopStmt),
    Tempo(f64, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
}
