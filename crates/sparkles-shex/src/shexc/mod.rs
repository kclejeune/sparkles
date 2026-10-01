//! ShExC, the compact syntax: a lossless lexer, a recursive-descent parser into the
//! [`crate::ast`], and a pretty printer.

pub mod lexer;
pub mod parser;
pub mod writer;
