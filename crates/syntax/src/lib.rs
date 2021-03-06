/// A span representing a section of source file
pub mod span;

/// Utilities
pub mod util;

/// Lexer for r0 tokens
pub mod lexer;
/// Models of r0 tokens
pub mod token;

/// Models of the abstract syntax tree.
pub mod ast;
/// Parser for r0 programs
pub mod parser;

/// Visitor trait for working with AST
pub mod visitor;

pub use lexer::Lexer;
pub use token::Token;

pub mod prelude {
    pub use crate::span::Span;
    pub use crate::util::{Mut, MutWeak, P};
}

pub fn parse(program: &str) -> Result<ast::Program, parser::err::ParseError> {
    let mut parser = parser::Parser::new(lexer::spanned_lexer(program));
    parser.parse()
}
