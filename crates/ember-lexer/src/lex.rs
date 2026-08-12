use crate::token::{Token, TokenKind};
use ember_diag::Diagnostic;
use ember_span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriviaKind {
    Line,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub span: Span,
}

/// Total function: never panics, never returns early. Unrecognized input
/// becomes TokenKind::Error with a diagnostic, and lexing continues — an
/// editor needs a full token stream for text that is malformed 100% of the
/// time it's being typed.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Trivia>, Vec<Diagnostic>) {
    let mut lx = Lexer::new(src);
    let mut tokens = Vec::with_capacity(src.len() / 4 + 1);
    loop {
        let tok = lx.next_token();
        let done = tok.kind == TokenKind::Eof;
        tokens.push(tok);
        if done {
            break;
        }
    }
    (tokens, lx.trivia, lx.diagnostics)
}

struct Lexer<'src> {
    src: &'src str,
    pos: u32,
    trivia: Vec<Trivia>,
    diagnostics: Vec<Diagnostic>,
}

impl<'src> Lexer<'src> {
    fn new(src: &'src str) -> Self {
        Lexer {
            src,
            pos: 0,
            trivia: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn at_end(&self) -> bool {
        self.pos as usize >= self.src.len()
    }

    fn rest(&self) -> &'src str {
        &self.src[self.pos as usize..]
    }

    fn peek(&self) -> char {
        self.rest().chars().next().unwrap_or('\0')
    }

    fn peek_at(&self, n: usize) -> char {
        self.rest().chars().nth(n).unwrap_or('\0')
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.rest().chars().next()?;
        self.pos += c.len_utf8() as u32;
        Some(c)
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == expected {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error(&mut self, span: Span, code: &'static str, msg: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(msg)
                .with_code(code)
                .with_primary(span, "here"),
        );
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                ' ' | '\t' | '\r' | '\n' => {
                    self.advance();
                }
                '/' if self.peek_at(1) == '/' => {
                    let start = self.pos;
                    while !self.at_end() && self.peek() != '\n' {
                        self.advance();
                    }
                    self.trivia.push(Trivia {
                        kind: TriviaKind::Line,
                        span: Span::new(start, self.pos),
                    });
                }
                '/' if self.peek_at(1) == '*' => {
                    let start = self.pos;
                    self.advance();
                    self.advance();
                    self.block_comment();
                    self.trivia.push(Trivia {
                        kind: TriviaKind::Block,
                        span: Span::new(start, self.pos),
                    });
                }
                _ => break,
            }
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos - 2; // back up over the opening "/*"
        let mut depth = 1usize;
        loop {
            if self.at_end() {
                self.error(
                    Span::new(start, self.pos),
                    "E0101",
                    "unterminated block comment",
                );
                return;
            }
            if self.peek() == '*' && self.peek_at(1) == '/' {
                self.advance();
                self.advance();
                depth -= 1;
                if depth == 0 {
                    return;
                }
            } else if self.peek() == '/' && self.peek_at(1) == '*' {
                self.advance();
                self.advance();
                depth += 1;
            } else {
                self.advance();
            }
        }
    }

    fn next_token(&mut self) -> Token {
        self.skip_trivia();
        let start = self.pos;
        let Some(c) = self.advance() else {
            return Token {
                kind: TokenKind::Eof,
                span: Span::new(start, start),
            };
        };

        let kind = match c {
            c if c.is_ascii_alphabetic() || c == '_' => self.ident_or_keyword(start),
            '0'..='9' => self.number(start),
            '"' => self.string(start),
            '=' if self.eat('=') => TokenKind::EqEq,
            '=' if self.eat('>') => TokenKind::FatArrow,
            '=' => TokenKind::Eq,
            '!' if self.eat('=') => TokenKind::BangEq,
            '!' => TokenKind::Bang,
            '<' if self.eat('=') => TokenKind::LtEq,
            '<' => TokenKind::Lt,
            '>' if self.eat('=') => TokenKind::GtEq,
            '>' => TokenKind::Gt,
            '-' if self.eat('>') => TokenKind::Arrow,
            '-' => TokenKind::Minus,
            '&' if self.eat('&') => TokenKind::AndAnd,
            '|' if self.eat('|') => TokenKind::OrOr,
            '|' => TokenKind::Pipe,
            ':' if self.eat(':') => TokenKind::ColonColon,
            ':' => TokenKind::Colon,
            '.' if self.eat('.') => TokenKind::DotDot,
            '.' => TokenKind::Dot,
            '+' => TokenKind::Plus,
            '*' => TokenKind::Star,
            '%' => TokenKind::Percent,
            '/' => TokenKind::Slash,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semi,
            other => {
                self.error(
                    Span::new(start, self.pos),
                    "E0102",
                    format!("unexpected character `{other}`"),
                );
                TokenKind::Error
            }
        };
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }

    fn ident_or_keyword(&mut self, start: u32) -> TokenKind {
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
            self.advance();
        }
        let text = &self.src[start as usize..self.pos as usize];
        TokenKind::keyword_from_str(text).unwrap_or(TokenKind::Ident)
    }

    fn number(&mut self, start: u32) -> TokenKind {
        // The leading digit was already consumed by `next_token` before
        // dispatching here, so `self.peek()` is the *second* character.
        // A radix prefix ("0x"/"0b"/"0o") is only possible when that first
        // digit was '0' — check it via `start` rather than re-peeking for it.
        let first_digit_is_zero = self.src.as_bytes()[start as usize] == b'0';
        if first_digit_is_zero && matches!(self.peek(), 'x' | 'X' | 'b' | 'B' | 'o' | 'O') {
            self.advance(); // radix marker
            while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                self.advance();
            }
            return TokenKind::Int;
        }

        while self.peek().is_ascii_digit() || self.peek() == '_' {
            self.advance();
        }

        // A '.' after digits is only a float if the NEXT char is also a digit —
        // otherwise `1..10` would lex as Float Dot Int instead of Int DotDot Int,
        // and `1.foo` would swallow the dot into a malformed float.
        if self.peek() == '.' && self.peek_at(1).is_ascii_digit() {
            self.advance();
            while self.peek().is_ascii_digit() || self.peek() == '_' {
                self.advance();
            }
            self.maybe_exponent();
            return TokenKind::Float;
        }
        if matches!(self.peek(), 'e' | 'E') {
            self.maybe_exponent();
            return TokenKind::Float;
        }
        TokenKind::Int
    }

    fn string(&mut self, start: u32) -> TokenKind {
        loop {
            if self.at_end() {
                self.error(
                    Span::new(start, start + 1),
                    "E0103",
                    "unterminated string literal",
                );
                return TokenKind::Error;
            }
            match self.advance().unwrap() {
                '"' => return TokenKind::Str,
                '\\' => {
                    // Consume the escaped character (or unicode escape body);
                    // validity of the escape is a later-phase concern, not the
                    // lexer's — it only needs to not desynchronize on `\"`.
                    match self.peek() {
                        'u' => {
                            self.advance();
                            if self.peek() == '{' {
                                self.advance();
                                while !self.at_end() && self.peek() != '}' {
                                    self.advance();
                                }
                                self.eat('}');
                            }
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn maybe_exponent(&mut self) {
        if matches!(self.peek(), 'e' | 'E') {
            self.advance();
            if matches!(self.peek(), '+' | '-') {
                self.advance();
            }
            while self.peek().is_ascii_digit() {
                self.advance();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_yields_just_eof() {
        let (tokens, _trivia, diags) = lex("");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Eof);
        assert!(diags.is_empty());
    }

    #[test]
    fn whitespace_only_yields_just_eof() {
        let (tokens, _trivia, _) = lex("   \t\n\n  ");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Eof);
    }

    #[test]
    fn eof_span_is_at_end_of_source() {
        let (tokens, _trivia, _) = lex("  ");
        assert_eq!(tokens[0].span, Span::new(2, 2));
    }

    #[test]
    fn identifier_lexes_as_ident() {
        let (tokens, _trivia, _) = lex("foo_bar1");
        assert_eq!(tokens[0].kind, TokenKind::Ident);
        assert_eq!(tokens[0].span, Span::new(0, 8));
    }

    #[test]
    fn keyword_lexes_as_its_own_kind() {
        let (tokens, _trivia, _) = lex("let");
        assert_eq!(tokens[0].kind, TokenKind::Let);
    }

    #[test]
    fn keyword_prefix_identifier_is_still_ident() {
        // "letter" must not be lexed as `let` + `ter`.
        let (tokens, _trivia, _) = lex("letter");
        assert_eq!(tokens[0].kind, TokenKind::Ident);
        assert_eq!(tokens[0].span, Span::new(0, 6));
    }

    #[test]
    fn true_false_nil_are_keywords() {
        let (tokens, _trivia, _) = lex("true false nil");
        assert_eq!(tokens[0].kind, TokenKind::True);
        assert_eq!(tokens[1].kind, TokenKind::False);
        assert_eq!(tokens[2].kind, TokenKind::Nil);
    }

    #[test]
    fn decimal_int() {
        let (tokens, _trivia, _) = lex("42");
        assert_eq!(tokens[0].kind, TokenKind::Int);
    }

    #[test]
    fn int_with_underscore_separators() {
        let (tokens, _trivia, diags) = lex("1_000_000");
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert!(diags.is_empty());
    }

    #[test]
    fn hex_bin_oct_ints() {
        let (tokens, _trivia, _) = lex("0xFF 0b101 0o17");
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert_eq!(tokens[1].kind, TokenKind::Int);
        assert_eq!(tokens[2].kind, TokenKind::Int);
    }

    #[test]
    fn float_basic() {
        let (tokens, _trivia, _) = lex("1.5");
        assert_eq!(tokens[0].kind, TokenKind::Float);
        assert_eq!(tokens[0].span, Span::new(0, 3));
    }

    #[test]
    fn float_with_exponent() {
        let (tokens, _trivia, _) = lex("1e10 1.5e-3");
        assert_eq!(tokens[0].kind, TokenKind::Float);
        assert_eq!(tokens[1].kind, TokenKind::Float);
    }

    #[test]
    fn range_is_not_a_float() {
        // 1..10 must lex the leading int as just "1" — NOT as a float that
        // swallows the dot(s). Operator dispatch for `.`/`..` doesn't exist
        // until a later task, so we can't assert the *following* token's kind
        // yet (it's still an Error token right now) — what matters here is
        // that `number()` stops at the right boundary.
        let (tokens, _trivia, _) = lex("1..10");
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert_eq!(tokens[0].span, Span::new(0, 1));
    }

    #[test]
    fn bare_dot_after_int_is_not_consumed_as_float() {
        // "1." with no trailing digit: the int must stop at "1", not swallow
        // the dot into a malformed float.
        let (tokens, _trivia, _) = lex("1.foo");
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert_eq!(tokens[0].span, Span::new(0, 1));
    }

    #[test]
    fn simple_string() {
        let (tokens, _trivia, diags) = lex(r#""hello""#);
        assert_eq!(tokens[0].kind, TokenKind::Str);
        assert!(diags.is_empty());
    }

    #[test]
    fn string_with_escapes() {
        let (tokens, _trivia, diags) = lex(r#""a\nb\t\"c\\""#);
        assert_eq!(tokens[0].kind, TokenKind::Str);
        assert!(diags.is_empty());
    }

    #[test]
    fn string_with_unicode_escape() {
        let (tokens, _trivia, diags) = lex(r#""\u{1F600}""#);
        assert_eq!(tokens[0].kind, TokenKind::Str);
        assert!(diags.is_empty());
    }

    #[test]
    fn unterminated_string_reports_at_opening_quote() {
        let (tokens, _trivia, diags) = lex("\"never closed");
        assert_eq!(tokens[0].kind, TokenKind::Error);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].primary_span(), Some(Span::new(0, 1)));
    }

    #[test]
    fn maximal_munch_two_char_operators() {
        let cases: &[(&str, TokenKind)] = &[
            ("==", TokenKind::EqEq),
            ("!=", TokenKind::BangEq),
            ("<=", TokenKind::LtEq),
            (">=", TokenKind::GtEq),
            ("&&", TokenKind::AndAnd),
            ("||", TokenKind::OrOr),
            ("->", TokenKind::Arrow),
            ("=>", TokenKind::FatArrow),
            ("..", TokenKind::DotDot),
            ("::", TokenKind::ColonColon),
        ];
        for (src, expected) in cases {
            let (tokens, _trivia, _) = lex(src);
            assert_eq!(tokens[0].kind, *expected, "lexing {src:?}");
            assert_eq!(tokens[0].span, Span::new(0, 2), "span for {src:?}");
        }
    }

    #[test]
    fn single_char_operators_and_delimiters() {
        let cases: &[(&str, TokenKind)] = &[
            ("+", TokenKind::Plus),
            ("-", TokenKind::Minus),
            ("*", TokenKind::Star),
            ("/", TokenKind::Slash),
            ("%", TokenKind::Percent),
            ("=", TokenKind::Eq),
            ("<", TokenKind::Lt),
            (">", TokenKind::Gt),
            ("!", TokenKind::Bang),
            ("|", TokenKind::Pipe),
            (".", TokenKind::Dot),
            (":", TokenKind::Colon),
            ("(", TokenKind::LParen),
            (")", TokenKind::RParen),
            ("{", TokenKind::LBrace),
            ("}", TokenKind::RBrace),
            ("[", TokenKind::LBracket),
            ("]", TokenKind::RBracket),
            (",", TokenKind::Comma),
            (";", TokenKind::Semi),
        ];
        for (src, expected) in cases {
            let (tokens, _trivia, _) = lex(src);
            assert_eq!(tokens[0].kind, *expected, "lexing {src:?}");
        }
    }

    #[test]
    fn equals_does_not_swallow_into_eqeq_wrongly() {
        // `a == b` must be Ident EqEq Ident, not Ident Eq Eq Ident.
        let (tokens, _trivia, _) = lex("a == b");
        assert_eq!(tokens[0].kind, TokenKind::Ident);
        assert_eq!(tokens[1].kind, TokenKind::EqEq);
        assert_eq!(tokens[2].kind, TokenKind::Ident);
    }

    #[test]
    fn nested_block_comment_depth_three_closes_correctly() {
        let (tokens, _trivia, diags) = lex("/* a /* b /* c */ d */ e */ let x = 1;");
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        assert_eq!(tokens[0].kind, TokenKind::Let);
    }

    #[test]
    fn unterminated_block_comment_reports_one_diagnostic() {
        let (_, _trivia, diags) = lex("/* never closed");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unterminated block comment"));
    }

    #[test]
    fn full_corpus_lexes_without_panicking() {
        let corpus = [
            "let x = 1;",
            "fn add(a: Int, b: Int) -> Int { a + b }",
            r#"let s = "hi\n";"#,
            "for i in 0..10 { print(i); }",
            "match s { Circle(r) => 1, _ => 0 }",
            "/* /* nested */ */ 1..10",
            "@@@ garbage $$$ input ###",
        ];
        for src in corpus {
            let (_tokens, _trivia, _diags) = lex(src);
        }
    }

    const CORPUS: &[&str] = &[
        "",
        "   ",
        "let x = 1;",
        "// comment\nlet x = 1;",
        "/* block */ let x = 1;",
        "a == b && c != d || e <= f",
        "fn f(x: i32) -> bool { x >= 0 }",
        "1..10",
        "a::b::c",
        "x = match y { _ => 1 };",
        "/* /* nested */ */ 1..10",
    ];

    #[test]
    fn line_and_block_comments_are_recorded_as_trivia() {
        let (tokens, trivia, diags) = lex("// leading\nlet x = 1; /* trailing */");
        assert!(diags.is_empty());
        assert_eq!(trivia.len(), 2, "{trivia:?}");
        assert_eq!(trivia[0].kind, TriviaKind::Line);
        assert_eq!(trivia[0].span, Span::new(0, 10)); // "// leading" (no trailing \n in the span)
        assert_eq!(trivia[1].kind, TriviaKind::Block);
        assert_eq!(trivia[1].span, Span::new(22, 36)); // "/* trailing */"
                                                       // The token stream itself must be completely unaffected by this
                                                       // change — same tokens, same spans, as before.
        assert_eq!(tokens[0].kind, TokenKind::Let);
    }

    #[test]
    fn no_comments_yields_empty_trivia() {
        let (_tokens, trivia, _diags) = lex("let x = 1;");
        assert!(trivia.is_empty());
    }

    #[test]
    fn spans_tile_the_source_exactly() {
        for src in CORPUS {
            let (tokens, _trivia, _) = lex(src);
            let mut cursor = 0u32;
            for t in &tokens {
                assert!(
                    t.span.start >= cursor,
                    "overlapping span at {:?} in {:?}",
                    t,
                    src
                );
                let gap = &src[cursor as usize..t.span.start as usize];
                assert!(
                    gap.chars().all(|c| c.is_whitespace())
                        || gap.contains("//")
                        || gap.contains("/*"),
                    "non-trivia gap {:?} before {:?} in {:?}",
                    gap,
                    t,
                    src
                );
                cursor = t.span.end;
            }
            assert_eq!(
                cursor as usize,
                src.len(),
                "tokens don't cover all of {:?}",
                src
            );
        }
    }
}
