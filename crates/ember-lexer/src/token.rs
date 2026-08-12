use ember_span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[repr(u8)]
pub enum TokenKind {
    // literals
    Int,
    Float,
    Str,
    True,
    False,
    Nil,
    Ident,
    // keywords
    Let,
    Mut,
    Fn,
    If,
    Else,
    While,
    For,
    In,
    Loop,
    Break,
    Continue,
    Return,
    Match,
    Type,
    Struct,
    Import,
    // operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,
    OrOr,
    Bang,
    Pipe,
    Arrow,
    FatArrow,
    DotDot,
    Dot,
    Colon,
    ColonColon,
    // delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    // trivia & control
    Comment,
    Eof,
    Error,
}

impl TokenKind {
    /// All reserved words are recognized here, via direct match on the
    /// identifier slice — never a HashMap lookup.
    pub fn keyword_from_str(s: &str) -> Option<TokenKind> {
        Some(match s {
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "fn" => TokenKind::Fn,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "return" => TokenKind::Return,
            "match" => TokenKind::Match,
            "type" => TokenKind::Type,
            "struct" => TokenKind::Struct,
            "import" => TokenKind::Import,
            "nil" => TokenKind::Nil,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_12_bytes() {
        assert_eq!(std::mem::size_of::<Token>(), 12);
    }

    #[test]
    fn keyword_from_str_recognizes_all_reserved_words() {
        let words = [
            "let", "mut", "fn", "if", "else", "while", "for", "in", "loop", "break", "continue",
            "return", "match", "type", "struct", "import", "nil", "true", "false",
        ];
        for w in words {
            assert!(
                TokenKind::keyword_from_str(w).is_some(),
                "{w} should be a keyword"
            );
        }
        assert_eq!(TokenKind::keyword_from_str("foobar"), None);
    }
}
