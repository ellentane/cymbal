#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Lex,
    Parse,
    Eval,
    Audio,
    Io,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub span: Span,
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(span: Span, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            span,
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            ErrorKind::Lex => "lex error",
            ErrorKind::Parse => "parse error",
            ErrorKind::Eval => "eval error",
            ErrorKind::Audio => "audio error",
            ErrorKind::Io => "io error",
        };
        write!(
            f,
            "line {}, col {}: {}: {}",
            self.span.line, self.span.col, kind, self.message
        )
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_displays_span_kind_and_message() {
        let e = Error::new(
            Span { line: 2, col: 7 },
            ErrorKind::Parse,
            "unexpected token",
        );
        assert_eq!(
            e.to_string(),
            "line 2, col 7: parse error: unexpected token"
        );
    }
}
