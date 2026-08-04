pub mod ast;
pub mod builtin_samples;
pub mod dsp;
pub mod error;
pub mod lexer;
pub mod midi;
pub mod mixer;
pub mod parser;
pub mod pattern;
pub mod render;
pub mod scheduler;
pub mod timefmt;
pub mod transform;
pub mod transport;
pub mod wav;

pub fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!crate_version().is_empty());
    }
}
