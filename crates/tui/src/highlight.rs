use cymbal_core::lexer::{TokenKind, lex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Plain,
    Keyword,
    Pattern,
    Note,
    Number,
    Comment,
}

#[allow(dead_code)]
pub fn classify(kind: &TokenKind) -> Class {
    match kind {
        TokenKind::Let | TokenKind::Loop | TokenKind::Tempo | TokenKind::Rev | TokenKind::Every => {
            Class::Keyword
        }
        TokenKind::String(_) => Class::Pattern,
        TokenKind::Note(_) => Class::Note,
        TokenKind::Number(_) => Class::Number,
        _ => Class::Plain,
    }
}

pub fn highlight_line(line: &str) -> Vec<(String, Class)> {
    let (code, comment) = match line.find("--") {
        Some(i) => (&line[..i], Some(&line[i..])),
        None => (line, None),
    };
    let mut spans: Vec<(String, Class)> = Vec::new();
    if !code.is_empty() {
        match lex(&format!("{code}\n")) {
            Ok(tokens) => {
                for t in &tokens {
                    if t.kind == TokenKind::Newline || t.kind == TokenKind::Eof {
                        continue;
                    }
                    if let TokenKind::String(s) = &t.kind {
                        spans.push((s.clone(), Class::Pattern));
                    } else {
                        spans.push((token_text(t), classify(&t.kind)));
                    }
                }
            }
            Err(_) => spans.push((code.to_string(), Class::Plain)),
        }
    }
    if let Some(c) = comment {
        spans.push((c.to_string(), Class::Comment));
    }
    if spans.is_empty() {
        spans.push((line.to_string(), Class::Plain));
    }
    spans
}

fn token_text(t: &cymbal_core::lexer::Token) -> String {
    match &t.kind {
        TokenKind::Ident(s) => s.clone(),
        TokenKind::Note(m) => format!("{m}"),
        TokenKind::Number(n) => format!("{n}"),
        TokenKind::Let => "let".into(),
        TokenKind::Loop => "loop".into(),
        TokenKind::Tempo => "tempo".into(),
        TokenKind::Rev => "rev".into(),
        TokenKind::Every => "every".into(),
        TokenKind::LParen => "(".into(),
        TokenKind::RParen => ")".into(),
        TokenKind::LBracket => "[".into(),
        TokenKind::RBracket => "]".into(),
        TokenKind::Assign => "=".into(),
        TokenKind::Colon => ":".into(),
        TokenKind::Bind => "<<".into(),
        TokenKind::Pipe => ">>".into(),
        TokenKind::Comma => ",".into(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cymbal_core::lexer::TokenKind;

    #[test]
    fn classifies_tokens() {
        assert_eq!(classify(&TokenKind::Let), Class::Keyword);
        assert_eq!(classify(&TokenKind::String("x".into())), Class::Pattern);
        assert_eq!(classify(&TokenKind::Note(60)), Class::Note);
        assert_eq!(classify(&TokenKind::Number(120.0)), Class::Number);
        assert_eq!(classify(&TokenKind::Ident("kick".into())), Class::Plain);
        assert_eq!(classify(&TokenKind::Comma), Class::Plain);
    }

    #[test]
    fn highlights_comment() {
        let line = "kick << \"x\" -- note";
        let spans = highlight_line(line);
        assert!(spans.iter().any(|(_, c)| *c == Class::Comment));
    }

    #[test]
    fn graceful_on_lex_errors() {
        let spans = highlight_line("\"unterminated");
        assert!(!spans.is_empty());
    }
}
