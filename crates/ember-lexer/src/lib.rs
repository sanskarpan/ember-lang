pub mod lex;
pub mod token;

pub use lex::{lex, Trivia, TriviaKind};
pub use token::{Token, TokenKind};
