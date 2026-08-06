use crate::error::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceKind {
    Kick,
    Snare,
    Hat,
    Bass,
    Lead,
    Sample,
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
    Sample(String, Span),
    Name(String, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Combinator {
    Rev,
    Every(u64, Box<Combinator>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Param {
    Const(f32),
    Ramp(f32, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BindStmt {
    pub voice: Expr,
    pub pattern: Expr,
    pub combinators: Vec<Combinator>,
    pub pan: Option<Param>,
    pub vel: Option<Param>,
    pub delay_send: Option<Param>,
    pub reverb_send: Option<Param>,
    pub bass: Option<Param>,
    pub treble: Option<Param>,
    pub comp: Option<Param>,
    pub swing: Option<Param>,
    pub start: Option<Param>,
    pub end: Option<Param>,
    pub dur: Option<Param>,
    pub cycle: Option<Param>,
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
    Help(Option<String>, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
}
