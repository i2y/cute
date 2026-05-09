//! Cute language syntax: lexer, parser, AST, source spans, and diagnostics.
//!
//! This crate is intentionally free of semantic analysis - everything here
//! deals with surface syntax only.

pub mod ast;
pub mod diag;
pub mod format;
pub mod lex;
pub mod parse;
pub mod span;
pub mod token;

pub use ast::Module;
pub use diag::Diagnostic;
pub use format::{FormatError, format_source};
pub use lex::{LexError, lex};
pub use parse::{ParseError, parse, parse_binding, parse_expression};
pub use span::{FileId, SourceMap, Span};
pub use token::{Token, TokenKind};
