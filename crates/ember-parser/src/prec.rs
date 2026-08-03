use ember_lexer::TokenKind;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Prec {
    None = 0,
    Assign,
    Or,
    And,
    Equality,
    Comparison,
    Term,
    Factor,
    Unary,
    Call,
    Primary,
}

impl Prec {
    /// One step down. This is the entire difference between left- and
    /// right-associative operators: left-assoc recurses at the SAME
    /// precedence (an equal-precedence operator fails the inner loop's
    /// `> min_prec` check and gets absorbed by the outer loop instead,
    /// producing left-nesting); right-assoc recurses one step LOWER (an
    /// equal-precedence operator now passes the inner call's check and
    /// nests to the right).
    pub fn lower(self) -> Prec {
        match self {
            Prec::None => Prec::None,
            Prec::Assign => Prec::None,
            Prec::Or => Prec::Assign,
            Prec::And => Prec::Or,
            Prec::Equality => Prec::And,
            Prec::Comparison => Prec::Equality,
            Prec::Term => Prec::Comparison,
            Prec::Factor => Prec::Term,
            Prec::Unary => Prec::Factor,
            Prec::Call => Prec::Unary,
            Prec::Primary => Prec::Call,
        }
    }
}

pub trait InfixPrec {
    fn infix_prec(self) -> Prec;
}

impl InfixPrec for TokenKind {
    /// Binding power when this token appears in INFIX position.
    fn infix_prec(self) -> Prec {
        match self {
            TokenKind::Eq => Prec::Assign,
            TokenKind::OrOr => Prec::Or,
            TokenKind::AndAnd => Prec::And,
            TokenKind::EqEq | TokenKind::BangEq => Prec::Equality,
            TokenKind::Lt | TokenKind::LtEq | TokenKind::Gt | TokenKind::GtEq => Prec::Comparison,
            // Range (`..`) binds at the same level as comparisons — Ember has
            // no expression forms that mix the two, so the exact tier doesn't
            // matter beyond "binds looser than arithmetic".
            TokenKind::DotDot => Prec::Comparison,
            TokenKind::Plus | TokenKind::Minus => Prec::Term,
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent => Prec::Factor,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::Dot => Prec::Call,
            _ => Prec::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_ordering() {
        assert!(Prec::None < Prec::Assign);
        assert!(Prec::Assign < Prec::Or);
        assert!(Prec::Or < Prec::And);
        assert!(Prec::And < Prec::Equality);
        assert!(Prec::Equality < Prec::Comparison);
        assert!(Prec::Comparison < Prec::Term);
        assert!(Prec::Term < Prec::Factor);
        assert!(Prec::Factor < Prec::Unary);
        assert!(Prec::Unary < Prec::Call);
        assert!(Prec::Call < Prec::Primary);
    }

    #[test]
    fn lower_steps_down_exactly_one_level() {
        assert_eq!(Prec::Or.lower(), Prec::Assign);
        assert_eq!(Prec::Assign.lower(), Prec::None);
    }

    #[test]
    fn infix_prec_table() {
        use ember_lexer::TokenKind::*;
        assert_eq!(Eq.infix_prec(), Prec::Assign);
        assert_eq!(OrOr.infix_prec(), Prec::Or);
        assert_eq!(AndAnd.infix_prec(), Prec::And);
        assert_eq!(EqEq.infix_prec(), Prec::Equality);
        assert_eq!(Lt.infix_prec(), Prec::Comparison);
        assert_eq!(Plus.infix_prec(), Prec::Term);
        assert_eq!(Star.infix_prec(), Prec::Factor);
        assert_eq!(Ident.infix_prec(), Prec::None);
    }
}
