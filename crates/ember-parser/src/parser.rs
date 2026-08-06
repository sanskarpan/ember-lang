use ember_ast::{
    AdtVariant, Ast, Expr, FieldDecl, Idx, Interner, MatchArm, Param, Pattern, Stmt, Symbol,
    TypeExpr,
};
use ember_diag::Diagnostic;
use ember_lexer::{Token, TokenKind};
use ember_span::Span;

use crate::prec::{InfixPrec, Prec};

const MAX_EXPR_DEPTH: u32 = 200;

pub struct Parser<'src> {
    src: &'src str,
    tokens: Vec<Token>,
    pos: usize,
    pub ast: Ast,
    pub interner: Interner,
    pub diagnostics: Vec<Diagnostic>,
    panicking: bool,
    depth: u32,
    /// Suppresses struct-literal parsing while true — set around if/while/for
    /// conditions and match scrutinees (later tasks) so `if a { ... }` parses
    /// `a` as a plain variable, not the start of `a { ... }` struct literal.
    no_struct_literal: bool,
}

impl<'src> Parser<'src> {
    pub fn new(src: &'src str, tokens: Vec<Token>) -> Self {
        Parser {
            src,
            tokens,
            pos: 0,
            ast: Ast::new(),
            interner: Interner::new(),
            diagnostics: Vec::new(),
            panicking: false,
            depth: 0,
            no_struct_literal: false,
        }
    }

    fn peek(&self) -> Token {
        self.tokens[self.pos]
    }

    fn at_end(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    fn advance(&mut self) -> Token {
        let t = self.peek();
        if !self.at_end() {
            self.pos += 1;
        }
        t
    }

    fn text(&self, span: Span) -> &'src str {
        &self.src[span.start as usize..span.end as usize]
    }

    /// Cascade suppression: a diagnostic is only recorded if we're not
    /// already unwinding from a prior error at this position.
    fn emit(&mut self, diag: Diagnostic) {
        if self.panicking {
            return;
        }
        self.panicking = true;
        self.diagnostics.push(diag);
    }

    /// Pratt parsing / precedence climbing: NUD (`prefix`) then absorb LED
    /// (`infix`) while the next token binds tighter than our caller allows.
    pub fn expr(&mut self, min_prec: Prec) -> Idx<Expr> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            let span = self.peek().span;
            self.emit(Diagnostic::error("expression nested too deeply").with_primary(span, "here"));
            self.depth -= 1;
            return self.ast.alloc_expr(Expr::Error, span);
        }

        let mut lhs = self.prefix();
        while self.peek().kind.infix_prec() > min_prec {
            lhs = self.infix(lhs);
        }
        self.depth -= 1;
        lhs
    }

    fn prefix(&mut self) -> Idx<Expr> {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Int => {
                let text = self.text(tok.span).replace('_', "");
                self.ast
                    .alloc_expr(Expr::Int(parse_int_literal(&text)), tok.span)
            }
            TokenKind::Float => {
                let text = self.text(tok.span).replace('_', "");
                let f: f64 = text.parse().unwrap_or(0.0);
                self.ast.alloc_expr(Expr::Float(f), tok.span)
            }
            TokenKind::True => self.ast.alloc_expr(Expr::Bool(true), tok.span),
            TokenKind::False => self.ast.alloc_expr(Expr::Bool(false), tok.span),
            TokenKind::Nil => self.ast.alloc_expr(Expr::Nil, tok.span),
            TokenKind::Str => {
                let text = self.text(tok.span);
                let decoded = parse_string_literal(text);
                let sym = self.interner.intern(&decoded);
                self.ast.alloc_expr(Expr::Str(sym), tok.span)
            }
            TokenKind::Ident => {
                let text = self.text(tok.span).to_string();
                let sym = self.interner.intern(&text);
                if !self.no_struct_literal && self.peek().kind == TokenKind::LBrace {
                    self.struct_literal(tok, sym)
                } else {
                    self.ast.alloc_expr(Expr::Var(sym), tok.span)
                }
            }
            TokenKind::Minus | TokenKind::Bang => {
                let operand = self.expr(Prec::Unary);
                let operand_span = self.ast.span_of_expr(operand);
                self.ast.alloc_expr(
                    Expr::Unary {
                        op: tok.kind,
                        operand,
                    },
                    tok.span.to(operand_span),
                )
            }
            TokenKind::LParen => {
                let inner = self.expr(Prec::None);
                let close = self.expect_close(tok, TokenKind::RParen);
                // Grouping doesn't produce its own node — it just controls precedence.
                let _ = close;
                inner
            }
            TokenKind::LBrace => self.block(tok),
            TokenKind::If => self.if_expr(tok),
            TokenKind::LBracket => {
                let mut items = Vec::new();
                if self.peek().kind != TokenKind::RBracket {
                    loop {
                        items.push(self.expr(Prec::None));
                        if self.peek().kind == TokenKind::Comma {
                            self.advance();
                            if self.peek().kind == TokenKind::RBracket {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect_close(tok, TokenKind::RBracket);
                self.ast
                    .alloc_expr(Expr::List { items }, tok.span.to(close.span))
            }
            TokenKind::Match => self.match_expr(tok),
            TokenKind::Pipe => {
                let params = self.lambda_params(tok);
                let body = self.expr(Prec::Assign.lower());
                let body_span = self.ast.span_of_expr(body);
                self.ast
                    .alloc_expr(Expr::Lambda { params, body }, tok.span.to(body_span))
            }
            TokenKind::OrOr => {
                // '||' lexes as one token — a lambda with zero params.
                let body = self.expr(Prec::Assign.lower());
                let body_span = self.ast.span_of_expr(body);
                self.ast.alloc_expr(
                    Expr::Lambda {
                        params: Vec::new(),
                        body,
                    },
                    tok.span.to(body_span),
                )
            }
            _ => {
                self.emit(
                    Diagnostic::error(format!("expected an expression, found `{:?}`", tok.kind))
                        .with_primary(tok.span, "here"),
                );
                self.ast.alloc_expr(Expr::Error, tok.span)
            }
        }
    }

    fn infix(&mut self, lhs: Idx<Expr>) -> Idx<Expr> {
        let op = self.advance();
        let prec = op.kind.infix_prec();
        let lhs_span = self.ast.span_of_expr(lhs);
        match op.kind {
            // LEFT-associative: recurse at THIS precedence (see Prec::lower's doc comment).
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Lt
            | TokenKind::LtEq
            | TokenKind::Gt
            | TokenKind::GtEq
            | TokenKind::DotDot
            | TokenKind::AndAnd
            | TokenKind::OrOr => {
                let rhs = self.expr(prec);
                let rhs_span = self.ast.span_of_expr(rhs);
                self.ast.alloc_expr(
                    Expr::Binary {
                        op: op.kind,
                        lhs,
                        rhs,
                    },
                    lhs_span.to(rhs_span),
                )
            }
            TokenKind::Eq => {
                let rhs = self.expr(prec.lower());
                if !self.is_valid_assign_target(lhs) {
                    self.emit(
                        Diagnostic::error("invalid assignment target")
                            .with_primary(lhs_span, "here")
                            .with_help(
                                "only variables, fields, and index expressions can be assigned to",
                            ),
                    );
                }
                let rhs_span = self.ast.span_of_expr(rhs);
                self.ast.alloc_expr(
                    Expr::Assign {
                        target: lhs,
                        value: rhs,
                    },
                    lhs_span.to(rhs_span),
                )
            }
            TokenKind::LParen => self.finish_call(lhs),
            TokenKind::LBracket => self.finish_index(lhs),
            TokenKind::Dot => self.finish_field(lhs),
            _ => unreachable!("infix_prec() > None but no infix arm for {:?}", op.kind),
        }
    }

    fn is_valid_assign_target(&self, idx: Idx<Expr>) -> bool {
        matches!(
            self.ast.expr(idx),
            Expr::Var(_) | Expr::Index { .. } | Expr::Field { .. }
        )
    }

    /// Unclosed `(`/`{`/`[` must report at the OPENING delimiter — "unexpected
    /// end of file" is technically true and useless in a large file.
    fn expect_close(&mut self, open: Token, close: TokenKind) -> Token {
        if self.peek().kind == close {
            return self.advance();
        }
        self.emit(
            Diagnostic::error(format!("unclosed `{:?}`", open.kind))
                .with_primary(open.span, "unclosed delimiter")
                .with_secondary(self.peek().span, "expected the matching close here"),
        );
        Token {
            kind: close,
            span: self.peek().span,
        }
    }

    fn finish_call(&mut self, callee: Idx<Expr>) -> Idx<Expr> {
        let callee_span = self.ast.span_of_expr(callee);
        let open = self.tokens[self.pos - 1]; // the '(' infix() already consumed
        let mut args = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            loop {
                args.push(self.expr(Prec::None));
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                    if self.peek().kind == TokenKind::RParen {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        let close = self.expect_close(open, TokenKind::RParen);
        self.ast
            .alloc_expr(Expr::Call { callee, args }, callee_span.to(close.span))
    }

    fn finish_index(&mut self, base: Idx<Expr>) -> Idx<Expr> {
        let base_span = self.ast.span_of_expr(base);
        let open = self.tokens[self.pos - 1];
        let index = self.expr(Prec::None);
        let close = self.expect_close(open, TokenKind::RBracket);
        self.ast
            .alloc_expr(Expr::Index { base, index }, base_span.to(close.span))
    }

    fn finish_field(&mut self, base: Idx<Expr>) -> Idx<Expr> {
        let base_span = self.ast.span_of_expr(base);
        let name_tok = self.advance();
        if name_tok.kind != TokenKind::Ident {
            self.emit(
                Diagnostic::error("expected a field name after `.`")
                    .with_primary(name_tok.span, "here"),
            );
            return self
                .ast
                .alloc_expr(Expr::Error, base_span.to(name_tok.span));
        }
        let name_text = self.text(name_tok.span).to_string();
        let sym = self.interner.intern(&name_text);
        self.ast
            .alloc_expr(Expr::Field { base, name: sym }, base_span.to(name_tok.span))
    }

    /// Called after an error to skip forward to a plausible restart point:
    /// either just past a `;`, or right before a token that can only start a
    /// new statement. Resets `panicking` so the next real statement parses
    /// cleanly instead of triggering more suppressed-but-still-broken parses.
    fn synchronize(&mut self) {
        self.panicking = false;
        while !self.at_end() {
            if self.pos > 0 && self.tokens[self.pos - 1].kind == TokenKind::Semi {
                return;
            }
            match self.peek().kind {
                TokenKind::Let
                | TokenKind::Fn
                | TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Loop
                | TokenKind::Return
                | TokenKind::Match
                | TokenKind::Type
                | TokenKind::Struct
                | TokenKind::RBrace => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// General-purpose "expect this token kind or report an error here" —
    /// used where there's no natural opening delimiter to anchor a
    /// `expect_close`-style unclosed-delimiter diagnostic on.
    fn expect(&mut self, kind: TokenKind, msg: impl Into<String>) -> Token {
        if self.peek().kind == kind {
            return self.advance();
        }
        let span = self.peek().span;
        let found = self.peek().kind;
        self.emit(Diagnostic::error(msg).with_primary(span, format!("found `{found:?}`")));
        Token { kind, span }
    }

    /// Gate for keyword-led statement forms inside a block (`let`, `fn`, …).
    fn starts_keyword_stmt(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Let
                | TokenKind::Fn
                | TokenKind::Type
                | TokenKind::Struct
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Loop
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
        )
    }

    /// An expression statement needs `;` unless it's in tail position or the
    /// expression itself ends in `}`. Extended with `Match` in a later task
    /// alongside `If`/`Block`.
    fn ends_without_semi_needed(&self, e: Idx<Expr>) -> bool {
        matches!(
            self.ast.expr(e),
            Expr::If { .. } | Expr::Block { .. } | Expr::Match { .. }
        )
    }

    fn block(&mut self, open: Token) -> Idx<Expr> {
        let mut stmts = Vec::new();
        let mut tail = None;
        while self.peek().kind != TokenKind::RBrace && !self.at_end() {
            if self.starts_keyword_stmt() {
                stmts.push(self.stmt());
                if self.panicking {
                    self.synchronize();
                }
                continue;
            }
            let e = self.expr(Prec::None);
            let e_span = self.ast.span_of_expr(e);
            if self.peek().kind == TokenKind::Semi {
                let semi = self.advance();
                stmts.push(self.ast.alloc_stmt(Stmt::ExprStmt(e), e_span.to(semi.span)));
            } else if self.peek().kind == TokenKind::RBrace {
                tail = Some(e);
                break;
            } else if self.ends_without_semi_needed(e) {
                stmts.push(self.ast.alloc_stmt(Stmt::ExprStmt(e), e_span));
            } else {
                self.emit(
                    Diagnostic::error("expected `;` after expression")
                        .with_primary(self.peek().span, format!("found `{:?}`", self.peek().kind)),
                );
                stmts.push(self.ast.alloc_stmt(Stmt::ExprStmt(e), e_span));
            }
            if self.panicking {
                self.synchronize();
            }
        }
        let close = self.expect_close(open, TokenKind::RBrace);
        self.ast
            .alloc_expr(Expr::Block { stmts, tail }, open.span.to(close.span))
    }

    /// Requires an opening `{` before parsing a block. Unlike `expect`, this
    /// does NOT proceed into `block()` on failure — doing so with a
    /// synthetic, unconsumed brace would let `block()` scan forward past
    /// unrelated following code (e.g. the next top-level `fn`) hunting for a
    /// `}` that was never opened.
    fn block_or_error(&mut self, what: &str) -> Idx<Expr> {
        if self.peek().kind == TokenKind::LBrace {
            let open = self.advance();
            self.block(open)
        } else {
            let span = self.peek().span;
            self.emit(
                Diagnostic::error(format!("expected `{{` to start {what}"))
                    .with_primary(span, format!("found `{:?}`", self.peek().kind)),
            );
            self.ast.alloc_expr(Expr::Error, span)
        }
    }

    /// Dispatches keyword-led statements. Only reached when
    /// `starts_keyword_stmt` says yes.
    fn stmt(&mut self) -> Idx<Stmt> {
        match self.peek().kind {
            TokenKind::Let => self.let_stmt(),
            TokenKind::Fn => self.fn_stmt(),
            TokenKind::Type => self.type_decl(),
            TokenKind::Struct => self.struct_decl(),
            TokenKind::While => self.while_stmt(),
            TokenKind::For => self.for_stmt(),
            TokenKind::Loop => self.loop_stmt(),
            TokenKind::Return => self.return_stmt(),
            TokenKind::Break => self.break_stmt(),
            TokenKind::Continue => self.continue_stmt(),
            _ => unreachable!("stmt() called without starts_keyword_stmt() being true"),
        }
    }

    /// Parses a type expression: a bare name (`Int`), a generic instantiation
    /// (`Option<Int>`), a list type (`[Int]`), or a function type
    /// (`(Int, Int) -> Int`).
    fn type_expr(&mut self) -> Idx<TypeExpr> {
        if self.peek().kind == TokenKind::LParen {
            return self.fn_type();
        }
        if self.peek().kind == TokenKind::LBracket {
            let open = self.advance();
            let elem = self.type_expr();
            let close = self.expect_close(open, TokenKind::RBracket);
            return self
                .ast
                .alloc_type_expr(TypeExpr::List(elem), open.span.to(close.span));
        }
        let tok = self.advance();
        if tok.kind != TokenKind::Ident {
            self.emit(
                Diagnostic::error(format!("expected a type, found `{:?}`", tok.kind))
                    .with_primary(tok.span, "here"),
            );
            return self.ast.alloc_type_expr(TypeExpr::Error, tok.span);
        }
        let text = self.text(tok.span).to_string();
        let name = self.interner.intern(&text);
        if self.peek().kind == TokenKind::Lt {
            let open = self.advance();
            let mut args = Vec::new();
            loop {
                args.push(self.type_expr());
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            let close = self.expect(TokenKind::Gt, "expected `>` to close generic arguments");
            return self
                .ast
                .alloc_type_expr(TypeExpr::Generic { name, args }, open.span.to(close.span));
        }
        self.ast.alloc_type_expr(TypeExpr::Name(name), tok.span)
    }

    fn fn_type(&mut self) -> Idx<TypeExpr> {
        let open = self.advance(); // consume '('
        let mut params = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            loop {
                params.push(self.type_expr());
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect_close(open, TokenKind::RParen);
        self.expect(TokenKind::Arrow, "expected `->` after parameter types");
        let ret = self.type_expr();
        let ret_span = self.ast.span_of_type_expr(ret);
        self.ast
            .alloc_type_expr(TypeExpr::Fun { params, ret }, open.span.to(ret_span))
    }

    fn let_stmt(&mut self) -> Idx<Stmt> {
        let let_tok = self.advance(); // 'let'
        let mutable = if self.peek().kind == TokenKind::Mut {
            self.advance();
            true
        } else {
            false
        };
        let name_tok = self.advance();
        let name = self.expect_field_name(name_tok);
        let ty = if self.peek().kind == TokenKind::Colon {
            self.advance();
            Some(self.type_expr())
        } else {
            None
        };
        self.expect(TokenKind::Eq, "expected `=` in `let` binding");
        let init = self.expr(Prec::None);
        let semi = self.expect(TokenKind::Semi, "expected `;` after `let` binding");
        self.ast.alloc_stmt(
            Stmt::Let {
                name,
                mutable,
                ty,
                init,
            },
            let_tok.span.to(semi.span),
        )
    }

    fn fn_stmt(&mut self) -> Idx<Stmt> {
        let fn_tok = self.advance(); // 'fn'
        let name_tok = self.advance();
        let name = self.expect_field_name(name_tok);
        let open = self.expect(TokenKind::LParen, "expected `(` after function name");
        let mut params = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            loop {
                let p_tok = self.advance();
                let p_name = self.expect_field_name(p_tok);
                let p_ty = if self.peek().kind == TokenKind::Colon {
                    self.advance();
                    Some(self.type_expr())
                } else {
                    None
                };
                params.push(Param {
                    name: p_name,
                    ty: p_ty,
                    span: p_tok.span,
                });
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect_close(open, TokenKind::RParen);
        let ret_ty = if self.peek().kind == TokenKind::Arrow {
            self.advance();
            Some(self.type_expr())
        } else {
            None
        };
        let body = self.block_or_error("the function body");
        let body_span = self.ast.span_of_expr(body);
        self.ast.alloc_stmt(
            Stmt::Fn {
                name,
                params,
                ret_ty,
                body,
            },
            fn_tok.span.to(body_span),
        )
    }

    fn type_decl(&mut self) -> Idx<Stmt> {
        let type_tok = self.advance(); // 'type'
        let name_tok = self.advance();
        let name = self.expect_field_name(name_tok);
        self.expect(TokenKind::Eq, "expected `=` after type name");
        if self.peek().kind == TokenKind::Pipe {
            self.advance(); // optional leading `|` before the first variant
        }
        let mut variants = Vec::new();
        loop {
            let v_tok = self.advance();
            let v_name = self.expect_field_name(v_tok);
            let mut payload = Vec::new();
            if self.peek().kind == TokenKind::LParen {
                let open = self.advance();
                if self.peek().kind != TokenKind::RParen {
                    loop {
                        payload.push(self.type_expr());
                        if self.peek().kind == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.expect_close(open, TokenKind::RParen);
            }
            variants.push(AdtVariant {
                name: v_name,
                payload,
            });
            if self.peek().kind == TokenKind::Pipe {
                self.advance();
            } else {
                break;
            }
        }
        let semi = self.expect(TokenKind::Semi, "expected `;` after type declaration");
        self.ast.alloc_stmt(
            Stmt::TypeDecl { name, variants },
            type_tok.span.to(semi.span),
        )
    }

    fn struct_decl(&mut self) -> Idx<Stmt> {
        let struct_tok = self.advance(); // 'struct'
        let name_tok = self.advance();
        let name = self.expect_field_name(name_tok);
        let open = self.expect(TokenKind::LBrace, "expected `{` after struct name");
        let mut fields = Vec::new();
        if self.peek().kind != TokenKind::RBrace {
            loop {
                let f_tok = self.advance();
                let f_name = self.expect_field_name(f_tok);
                self.expect(TokenKind::Colon, "expected `:` after field name");
                let f_ty = self.type_expr();
                fields.push(FieldDecl {
                    name: f_name,
                    ty: f_ty,
                });
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                    if self.peek().kind == TokenKind::RBrace {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let close = self.expect_close(open, TokenKind::RBrace);
        self.ast.alloc_stmt(
            Stmt::StructDecl { name, fields },
            struct_tok.span.to(close.span),
        )
    }

    fn while_stmt(&mut self) -> Idx<Stmt> {
        let while_tok = self.advance();
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let cond = self.expr(Prec::None);
        self.no_struct_literal = prev;
        let body = self.block_or_error("the `while` body");
        let body_span = self.ast.span_of_expr(body);
        self.ast
            .alloc_stmt(Stmt::While { cond, body }, while_tok.span.to(body_span))
    }

    fn for_stmt(&mut self) -> Idx<Stmt> {
        let for_tok = self.advance();
        let binding_tok = self.advance();
        let binding = self.expect_field_name(binding_tok);
        self.expect(TokenKind::In, "expected `in` after `for` binding");
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let iter = self.expr(Prec::None);
        self.no_struct_literal = prev;
        let body = self.block_or_error("the `for` body");
        let body_span = self.ast.span_of_expr(body);
        self.ast.alloc_stmt(
            Stmt::For {
                binding,
                iter,
                body,
            },
            for_tok.span.to(body_span),
        )
    }

    fn loop_stmt(&mut self) -> Idx<Stmt> {
        let loop_tok = self.advance();
        let body = self.block_or_error("the `loop` body");
        let body_span = self.ast.span_of_expr(body);
        self.ast
            .alloc_stmt(Stmt::Loop { body }, loop_tok.span.to(body_span))
    }

    fn break_stmt(&mut self) -> Idx<Stmt> {
        let tok = self.advance();
        let semi = self.expect(TokenKind::Semi, "expected `;` after `break`");
        self.ast.alloc_stmt(Stmt::Break, tok.span.to(semi.span))
    }

    fn continue_stmt(&mut self) -> Idx<Stmt> {
        let tok = self.advance();
        let semi = self.expect(TokenKind::Semi, "expected `;` after `continue`");
        self.ast.alloc_stmt(Stmt::Continue, tok.span.to(semi.span))
    }

    fn return_stmt(&mut self) -> Idx<Stmt> {
        let tok = self.advance();
        let value = if self.peek().kind == TokenKind::Semi {
            None
        } else {
            Some(self.expr(Prec::None))
        };
        let semi = self.expect(TokenKind::Semi, "expected `;` after `return`");
        self.ast
            .alloc_stmt(Stmt::Return(value), tok.span.to(semi.span))
    }

    fn if_expr(&mut self, if_tok: Token) -> Idx<Expr> {
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let cond = self.expr(Prec::None);
        self.no_struct_literal = prev;
        let then_ = self.block_or_error("the `if` body");
        let else_ = if self.peek().kind == TokenKind::Else {
            self.advance();
            if self.peek().kind == TokenKind::If {
                let inner_if = self.advance();
                Some(self.if_expr(inner_if))
            } else {
                Some(self.block_or_error("the `else` body"))
            }
        } else {
            None
        };
        let end_span = else_
            .map(|e| self.ast.span_of_expr(e))
            .unwrap_or_else(|| self.ast.span_of_expr(then_));
        self.ast
            .alloc_expr(Expr::If { cond, then_, else_ }, if_tok.span.to(end_span))
    }

    fn lambda_params(&mut self, open: Token) -> Vec<Param> {
        let mut params = Vec::new();
        if self.peek().kind != TokenKind::Pipe {
            loop {
                let p = self.advance();
                if p.kind != TokenKind::Ident {
                    self.emit(
                        Diagnostic::error("expected a parameter name").with_primary(p.span, "here"),
                    );
                } else {
                    let text = self.text(p.span).to_string();
                    let sym = self.interner.intern(&text);
                    params.push(Param {
                        name: sym,
                        ty: None,
                        span: p.span,
                    });
                }
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect_close(open, TokenKind::Pipe);
        params
    }

    fn struct_literal(&mut self, name_tok: Token, name: Symbol) -> Idx<Expr> {
        let open = self.advance(); // consume '{'
        let mut fields = Vec::new();
        if self.peek().kind != TokenKind::RBrace {
            loop {
                let field_tok = self.advance();
                let field_sym = self.expect_field_name(field_tok);
                self.expect(TokenKind::Colon, "expected `:` after field name");
                let value = self.expr(Prec::None);
                fields.push((field_sym, value));
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                    if self.peek().kind == TokenKind::RBrace {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let close = self.expect_close(open, TokenKind::RBrace);
        self.ast
            .alloc_expr(Expr::Struct { name, fields }, name_tok.span.to(close.span))
    }

    fn expect_field_name(&mut self, tok: Token) -> Symbol {
        if tok.kind == TokenKind::Ident {
            let t = self.text(tok.span).to_string();
            self.interner.intern(&t)
        } else {
            self.emit(Diagnostic::error("expected a field name").with_primary(tok.span, "here"));
            self.interner.intern("<error>")
        }
    }

    fn match_expr(&mut self, match_tok: Token) -> Idx<Expr> {
        let prev = self.no_struct_literal;
        self.no_struct_literal = true;
        let scrutinee = self.expr(Prec::None);
        self.no_struct_literal = prev;
        let open = self.expect(TokenKind::LBrace, "expected `{` to start match arms");
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace && !self.at_end() {
            let pat = self.pattern();
            let guard = if self.peek().kind == TokenKind::If {
                self.advance();
                Some(self.expr(Prec::None))
            } else {
                None
            };
            self.expect(TokenKind::FatArrow, "expected `=>` after match pattern");
            let body = self.expr(Prec::None);
            let pat_span = self.ast.span_of_pat(pat);
            let body_span = self.ast.span_of_expr(body);
            arms.push(MatchArm {
                pat,
                guard,
                body,
                span: pat_span.to(body_span),
            });
            if self.peek().kind == TokenKind::Comma {
                self.advance();
            }
        }
        let close = self.expect_close(open, TokenKind::RBrace);
        self.ast.alloc_expr(
            Expr::Match { scrutinee, arms },
            match_tok.span.to(close.span),
        )
    }

    fn pattern(&mut self) -> Idx<Pattern> {
        let first = self.pattern_primary();
        if self.peek().kind != TokenKind::Pipe {
            return first;
        }
        let mut alts = vec![first];
        while self.peek().kind == TokenKind::Pipe {
            self.advance();
            alts.push(self.pattern_primary());
        }
        let start = self.ast.span_of_pat(alts[0]);
        let end = self.ast.span_of_pat(*alts.last().unwrap());
        self.ast.alloc_pat(Pattern::Or(alts), start.to(end))
    }

    fn pattern_primary(&mut self) -> Idx<Pattern> {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Ident if self.text(tok.span) == "_" => {
                self.ast.alloc_pat(Pattern::Wild, tok.span)
            }
            TokenKind::Ident => {
                let text = self.text(tok.span).to_string();
                let sym = self.interner.intern(&text);
                match self.peek().kind {
                    TokenKind::LParen => {
                        self.advance();
                        let mut args = Vec::new();
                        if self.peek().kind != TokenKind::RParen {
                            loop {
                                args.push(self.pattern());
                                if self.peek().kind == TokenKind::Comma {
                                    self.advance();
                                } else {
                                    break;
                                }
                            }
                        }
                        let close = self.expect_close(tok, TokenKind::RParen);
                        self.ast
                            .alloc_pat(Pattern::Ctor { name: sym, args }, tok.span.to(close.span))
                    }
                    TokenKind::LBrace => self.record_pattern(tok, sym),
                    _ => self.ast.alloc_pat(Pattern::Bind(sym), tok.span),
                }
            }
            TokenKind::Int => {
                let text = self.text(tok.span).replace('_', "");
                self.ast
                    .alloc_pat(Pattern::Int(parse_int_literal(&text)), tok.span)
            }
            TokenKind::True => self.ast.alloc_pat(Pattern::Bool(true), tok.span),
            TokenKind::False => self.ast.alloc_pat(Pattern::Bool(false), tok.span),
            TokenKind::LBracket => self.list_pattern(tok),
            TokenKind::LParen => {
                // `(pat)` with no comma is just grouping, same as an
                // expression in parens — it returns the inner pattern
                // unwrapped, not a one-element tuple. A trailing comma
                // (`(a,)`) or two-or-more items makes it a real tuple.
                let mut items = Vec::new();
                let mut saw_comma = false;
                if self.peek().kind != TokenKind::RParen {
                    loop {
                        items.push(self.pattern());
                        if self.peek().kind == TokenKind::Comma {
                            self.advance();
                            saw_comma = true;
                            if self.peek().kind == TokenKind::RParen {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                let close = self.expect_close(tok, TokenKind::RParen);
                if items.len() == 1 && !saw_comma {
                    items.into_iter().next().unwrap()
                } else {
                    self.ast
                        .alloc_pat(Pattern::Tuple(items), tok.span.to(close.span))
                }
            }
            _ => {
                self.emit(
                    Diagnostic::error(format!("expected a pattern, found `{:?}`", tok.kind))
                        .with_primary(tok.span, "here"),
                );
                self.ast.alloc_pat(Pattern::Error, tok.span)
            }
        }
    }

    fn list_pattern(&mut self, open: Token) -> Idx<Pattern> {
        let mut items = Vec::new();
        let mut rest = None;
        if self.peek().kind != TokenKind::RBracket {
            loop {
                if self.peek().kind == TokenKind::DotDot {
                    self.advance();
                    rest = Some(self.pattern_primary());
                    break;
                }
                items.push(self.pattern());
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        let close = self.expect_close(open, TokenKind::RBracket);
        self.ast
            .alloc_pat(Pattern::List { items, rest }, open.span.to(close.span))
    }

    fn record_pattern(&mut self, name_tok: Token, name: Symbol) -> Idx<Pattern> {
        let open = self.advance(); // consume '{'
        let mut fields = Vec::new();
        if self.peek().kind != TokenKind::RBrace {
            loop {
                let field_tok = self.advance();
                let field_sym = self.expect_field_name(field_tok);
                let pat = if self.peek().kind == TokenKind::Colon {
                    self.advance();
                    self.pattern()
                } else {
                    // shorthand `{ x }` binds `x` to a variable named `x`.
                    self.ast.alloc_pat(Pattern::Bind(field_sym), field_tok.span)
                };
                fields.push((field_sym, pat));
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        let close = self.expect_close(open, TokenKind::RBrace);
        self.ast.alloc_pat(
            Pattern::Record { name, fields },
            name_tok.span.to(close.span),
        )
    }

    /// A top-level (non-keyword-led) statement: an expression followed by
    /// `;`, mirroring the same-shaped logic inside `block()` for
    /// expression-statements at block scope.
    fn expr_stmt(&mut self) -> Idx<Stmt> {
        let e = self.expr(Prec::None);
        let e_span = self.ast.span_of_expr(e);
        if self.peek().kind == TokenKind::Semi {
            let semi = self.advance();
            self.ast.alloc_stmt(Stmt::ExprStmt(e), e_span.to(semi.span))
        } else if self.ends_without_semi_needed(e) {
            self.ast.alloc_stmt(Stmt::ExprStmt(e), e_span)
        } else {
            self.emit(
                Diagnostic::error("expected `;` after expression")
                    .with_primary(self.peek().span, format!("found `{:?}`", self.peek().kind)),
            );
            self.ast.alloc_stmt(Stmt::ExprStmt(e), e_span)
        }
    }
}

/// Decodes a lexed string literal's raw source text (including its
/// surrounding double quotes) into its logical contents, resolving the
/// escapes the lexer intentionally left unvalidated: `\n \t \r \\ \" \0`
/// and `\u{XXXX}`. Any other escaped character passes through literally —
/// escape validation is a later-phase concern, same as the lexer's.
fn parse_string_literal(text: &str) -> String {
    let inner = &text[1..text.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('0') => out.push('\0'),
            Some('u') => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut hex = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == '}' {
                            break;
                        }
                        hex.push(c);
                        chars.next();
                    }
                    chars.next(); // consume closing '}'
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                        }
                    }
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn parse_int_literal(text: &str) -> i64 {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).unwrap_or(0)
    } else if let Some(bin) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        i64::from_str_radix(bin, 2).unwrap_or(0)
    } else if let Some(oct) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        i64::from_str_radix(oct, 8).unwrap_or(0)
    } else {
        text.parse().unwrap_or(0)
    }
}

/// The main pipeline entry point: lex + parse a whole program into a flat
/// list of top-level statements. Always returns a complete list, however
/// malformed the input — a later task adds synchronize()-based recovery
/// that makes this true even when a statement in the middle fails to parse.
pub fn parse(src: &str) -> (Ast, Interner, Vec<Idx<Stmt>>, Vec<Diagnostic>) {
    let (tokens, lex_diags) = ember_lexer::lex(src);
    let mut p = Parser::new(src, tokens);
    p.diagnostics.extend(lex_diags);
    let mut stmts = Vec::new();
    while !p.at_end() {
        let s = if p.starts_keyword_stmt() {
            p.stmt()
        } else {
            p.expr_stmt()
        };
        stmts.push(s);
        if p.panicking {
            p.synchronize();
        }
    }
    (p.ast, p.interner, stmts, p.diagnostics)
}

/// Test/dev helper: lex + parse a single expression from source. Only
/// exercised by this module's own tests today, hence `#[cfg(test)]` —
/// remove that gate if a later task grows a non-test caller within the
/// crate.
#[cfg(test)]
pub(crate) fn parse_expr_from_str(src: &str) -> (Ast, Interner, Idx<Expr>, Vec<Diagnostic>) {
    let (tokens, lex_diags) = ember_lexer::lex(src);
    let mut p = Parser::new(src, tokens);
    p.diagnostics.extend(lex_diags);
    let e = p.expr(Prec::None);
    (p.ast, p.interner, e, p.diagnostics)
}

/// Test/dev helper: lex + parse a single whole statement from source. Same
/// rationale as `parse_expr_from_str` above for the `#[cfg(test)]` gate —
/// only exercised by this module's own tests today.
#[cfg(test)]
pub(crate) fn parse_stmt_from_str(src: &str) -> (Ast, Interner, Idx<Stmt>, Vec<Diagnostic>) {
    let (tokens, lex_diags) = ember_lexer::lex(src);
    let mut p = Parser::new(src, tokens);
    p.diagnostics.extend(lex_diags);
    let s = p.stmt();
    (p.ast, p.interner, s, p.diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_ast::Pattern;

    fn sexpr(src: &str) -> String {
        let (ast, interner, e, diags) = parse_expr_from_str(src);
        assert!(
            diags.is_empty(),
            "unexpected diagnostics for {src:?}: {diags:?}"
        );
        ember_ast::print_expr(&ast, &interner, e)
    }

    #[test]
    fn literals_and_identifiers_parse() {
        assert_eq!(sexpr("42"), "42");
        assert_eq!(sexpr("true"), "true");
        assert_eq!(sexpr("count"), "count");
    }

    #[test]
    fn string_literal_parses_with_escapes_decoded() {
        let (ast, interner, e, diags) = parse_expr_from_str(r#""hello\nworld""#);
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Str(sym) => assert_eq!(interner.resolve(*sym), "hello\nworld"),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[test]
    fn precedence() {
        assert_eq!(sexpr("1 + 2 * 3"), "(1 + (2 * 3))");
    }

    #[test]
    fn left_associativity() {
        assert_eq!(sexpr("1 - 2 - 3"), "((1 - 2) - 3)");
    }

    #[test]
    fn right_associativity_of_assignment() {
        assert_eq!(sexpr("a = b = c"), "(a = (b = c))");
    }

    #[test]
    fn invalid_assignment_target_is_an_error() {
        let (_, _, _, diags) = parse_expr_from_str("1 = 2");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("invalid assignment target"));
    }

    #[test]
    fn valid_assignment_targets_produce_no_error() {
        for src in ["a = 1", "a[0] = 1", "a.b = 1"] {
            let (_, _, _, diags) = parse_expr_from_str(src);
            assert!(
                diags.is_empty(),
                "unexpected diagnostics for {src:?}: {diags:?}"
            );
        }
    }

    #[test]
    fn unary_binds_tighter_than_binary() {
        assert_eq!(sexpr("-a + b"), "((-a) + b)");
    }

    #[test]
    fn call_binds_tightest() {
        assert_eq!(sexpr("-f(x)"), "(-(call f x))");
    }

    #[test]
    fn grouping_overrides_precedence() {
        assert_eq!(sexpr("(1 + 2) * 3"), "((1 + 2) * 3)");
    }

    #[test]
    fn index_and_field_access() {
        assert_eq!(sexpr("a[0]"), "(index a 0)");
        assert_eq!(sexpr("a.b"), "(field a b)");
    }

    #[test]
    fn call_with_multiple_args() {
        assert_eq!(sexpr("f(1, 2, 3)"), "(call f 1 2 3)");
    }

    #[test]
    fn call_with_trailing_comma() {
        assert_eq!(sexpr("f(1, 2,)"), "(call f 1 2)");
    }

    #[test]
    fn unclosed_paren_reports_at_opener() {
        let (_, _, _, diags) = parse_expr_from_str("(1 + 2");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unclosed"));
        assert_eq!(diags[0].primary_span(), Some(Span::new(0, 1)));
    }

    #[test]
    fn empty_block_has_no_stmts_or_tail() {
        let (ast, _interner, e, diags) = parse_expr_from_str("{ }");
        assert!(diags.is_empty());
        match ast.expr(e) {
            Expr::Block { stmts, tail } => {
                assert!(stmts.is_empty());
                assert!(tail.is_none());
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn block_with_tail_expression() {
        let (ast, interner, e, diags) = parse_expr_from_str("{ 1; 2 }");
        assert!(diags.is_empty());
        match ast.expr(e) {
            Expr::Block { stmts, tail } => {
                assert_eq!(stmts.len(), 1);
                assert_eq!(ember_ast::print_expr(&ast, &interner, tail.unwrap()), "2");
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn missing_semicolon_between_statements_is_one_error() {
        let (_, _, _, diags) = parse_expr_from_str("{ 1 2 }");
        assert_eq!(diags.len(), 1, "diags: {diags:?}");
        assert!(diags[0].message.contains("expected `;`"));
    }

    #[test]
    fn if_else_as_expression() {
        assert_eq!(sexpr("if a { 1 } else { 2 }"), "(if a (block 1) (block 2))");
    }

    #[test]
    fn if_without_else_prints_empty_else() {
        assert_eq!(sexpr("if a { 1 }"), "(if a (block 1) ())");
    }

    #[test]
    fn else_if_chains() {
        assert_eq!(
            sexpr("if a { 1 } else if b { 2 } else { 3 }"),
            "(if a (block 1) (if b (block 2) (block 3)))"
        );
    }

    #[test]
    fn lambda_with_params() {
        let (_, _, _, diags) = parse_expr_from_str("|x, y| x");
        assert!(diags.is_empty(), "diags: {diags:?}");
    }

    #[test]
    fn lambda_with_zero_params() {
        let (_, _, _, diags) = parse_expr_from_str("|| 1");
        assert!(diags.is_empty(), "diags: {diags:?}");
    }

    #[test]
    fn list_literal_parses_items() {
        let (ast, _interner, e, diags) = parse_expr_from_str("[1, 2, 3]");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::List { items } => assert_eq!(items.len(), 3),
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn empty_list_literal() {
        let (ast, _interner, e, diags) = parse_expr_from_str("[]");
        assert!(diags.is_empty());
        match ast.expr(e) {
            Expr::List { items } => assert!(items.is_empty()),
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn struct_literal_parses_outside_condition_position() {
        let (ast, _interner, e, diags) = parse_expr_from_str("Point { x: 1.0, y: 2.0 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Struct { fields, .. } => assert_eq!(fields.len(), 2),
            other => panic!("expected Struct, got {other:?}"),
        }
    }

    #[test]
    fn if_condition_identifier_is_not_parsed_as_struct_literal() {
        let (ast, _interner, e, diags) = parse_expr_from_str("if a { 1 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::If { cond, .. } => assert!(matches!(ast.expr(*cond), Expr::Var(_))),
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn match_with_ctor_and_wildcard_patterns() {
        let (ast, _interner, e, diags) =
            parse_expr_from_str("match s { Circle(r) => 1, Rect(w, h) => 2, _ => 0 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Match { arms, .. } => {
                assert_eq!(arms.len(), 3);
                assert!(matches!(ast.pat(arms[0].pat), Pattern::Ctor { .. }));
                assert!(matches!(ast.pat(arms[2].pat), Pattern::Wild));
            }
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn match_arm_with_guard() {
        let (ast, _interner, e, diags) = parse_expr_from_str("match x { n if n > 0 => 1, _ => 0 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Match { arms, .. } => assert!(arms[0].guard.is_some()),
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn or_pattern() {
        let (ast, _interner, e, diags) = parse_expr_from_str("match x { 1 | 2 => 0, _ => 1 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Match { arms, .. } => assert!(matches!(ast.pat(arms[0].pat), Pattern::Or(_))),
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn list_pattern_with_rest() {
        let (ast, _interner, e, diags) =
            parse_expr_from_str("match xs { [head, ..tail] => 1, [] => 0 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Match { arms, .. } => match ast.pat(arms[0].pat) {
                Pattern::List { items, rest } => {
                    assert_eq!(items.len(), 1);
                    assert!(rest.is_some());
                }
                other => panic!("expected List pattern, got {other:?}"),
            },
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn record_pattern_shorthand_and_explicit() {
        let (ast, _interner, e, diags) = parse_expr_from_str("match p { Point { x, y: yy } => 1 }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Match { arms, .. } => match ast.pat(arms[0].pat) {
                Pattern::Record { fields, .. } => assert_eq!(fields.len(), 2),
                other => panic!("expected Record pattern, got {other:?}"),
            },
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn let_with_type_annotation_and_mut() {
        let (ast, interner, stmt, diags) = parse_stmt_from_str("let mut count: Int = 0;");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::Let {
                name,
                mutable,
                ty,
                init,
            } => {
                assert_eq!(interner.resolve(*name), "count");
                assert!(mutable);
                assert!(ty.is_some());
                assert_eq!(ember_ast::print_expr(&ast, &interner, *init), "0");
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn fn_with_params_and_return_type() {
        let (ast, _interner, stmt, diags) =
            parse_stmt_from_str("fn add(a: Int, b: Int) -> Int { a + b }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::Fn { params, ret_ty, .. } => {
                assert_eq!(params.len(), 2);
                assert!(ret_ty.is_some());
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn fn_with_no_return_type_or_param_types() {
        let (ast, _interner, stmt, diags) = parse_stmt_from_str("fn identity(x) { x }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::Fn { params, ret_ty, .. } => {
                assert_eq!(params.len(), 1);
                assert!(params[0].ty.is_none());
                assert!(ret_ty.is_none());
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn fn_param_has_a_span_covering_its_name() {
        let (ast, _interner, stmt, diags) = parse_stmt_from_str("fn f(x: Int) { x }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::Fn { params, .. } => {
                assert_eq!(params[0].span, Span::new(5, 6)); // the "x" in "fn f(x: Int)"
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn lambda_params_are_now_full_params_with_spans() {
        let (ast, interner, e, diags) = parse_expr_from_str("|x, y| x");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.expr(e) {
            Expr::Lambda { params, .. } => {
                assert_eq!(params.len(), 2);
                assert_eq!(interner.resolve(params[0].name), "x");
                assert_eq!(params[0].span, Span::new(1, 2));
                assert!(
                    params[0].ty.is_none(),
                    "lambda params never have type annotations"
                );
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    #[test]
    fn adt_type_declaration_with_payloads() {
        let (ast, _interner, stmt, diags) =
            parse_stmt_from_str("type Shape = | Circle(Float) | Rect(Float, Float) | Point;");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::TypeDecl { variants, .. } => {
                assert_eq!(variants.len(), 3);
                assert_eq!(variants[0].payload.len(), 1);
                assert_eq!(variants[1].payload.len(), 2);
                assert_eq!(variants[2].payload.len(), 0);
            }
            other => panic!("expected TypeDecl, got {other:?}"),
        }
    }

    #[test]
    fn struct_declaration_with_typed_fields() {
        let (ast, _interner, stmt, diags) =
            parse_stmt_from_str("struct Point { x: Float, y: Float }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::StructDecl { fields, .. } => assert_eq!(fields.len(), 2),
            other => panic!("expected StructDecl, got {other:?}"),
        }
    }

    #[test]
    fn list_and_function_type_syntax() {
        let (ast, _interner, stmt, diags) =
            parse_stmt_from_str("fn f(xs: [Int]) -> (Int, Int) -> Int { xs }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::Fn { params, ret_ty, .. } => {
                assert!(matches!(
                    ast.type_expr(params[0].ty.unwrap()),
                    ember_ast::TypeExpr::List(_)
                ));
                assert!(matches!(
                    ast.type_expr(ret_ty.unwrap()),
                    ember_ast::TypeExpr::Fun { .. }
                ));
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn generic_type_syntax() {
        let (ast, _interner, stmt, diags) = parse_stmt_from_str("let x: Option<Int> = none;");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::Let { ty, .. } => {
                assert!(matches!(
                    ast.type_expr(ty.unwrap()),
                    ember_ast::TypeExpr::Generic { .. }
                ))
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn while_loop() {
        let (ast, _interner, stmt, diags) = parse_stmt_from_str("while a { b; }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        assert!(matches!(ast.stmt(stmt), Stmt::While { .. }));
    }

    #[test]
    fn for_loop_over_range() {
        let (ast, _interner, stmt, diags) = parse_stmt_from_str("for i in 0..10 { print(i); }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        match ast.stmt(stmt) {
            Stmt::For { iter, .. } => assert!(matches!(ast.expr(*iter), Expr::Binary { .. })),
            other => panic!("expected For, got {other:?}"),
        }
    }

    #[test]
    fn bare_loop() {
        let (ast, _interner, stmt, diags) = parse_stmt_from_str("loop { break; }");
        assert!(diags.is_empty(), "diags: {diags:?}");
        assert!(matches!(ast.stmt(stmt), Stmt::Loop { .. }));
    }

    #[test]
    fn break_continue_return() {
        let (ast, _interner, s1, d1) = parse_stmt_from_str("break;");
        assert!(d1.is_empty());
        assert!(matches!(ast.stmt(s1), Stmt::Break));

        let (ast2, _interner2, s2, d2) = parse_stmt_from_str("continue;");
        assert!(d2.is_empty());
        assert!(matches!(ast2.stmt(s2), Stmt::Continue));

        let (ast3, _interner3, s3, d3) = parse_stmt_from_str("return 1;");
        assert!(d3.is_empty());
        assert!(matches!(ast3.stmt(s3), Stmt::Return(Some(_))));

        let (ast4, _interner4, s4, d4) = parse_stmt_from_str("return;");
        assert!(d4.is_empty());
        assert!(matches!(ast4.stmt(s4), Stmt::Return(None)));
    }

    #[test]
    fn full_program_with_multiple_top_level_statements() {
        let src = r#"
            fn add(a: Int, b: Int) -> Int { a + b }
            let x = add(1, 2);
            print(x);
        "#;
        let (ast, _interner, stmts, diags) = parse(src);
        assert!(diags.is_empty(), "diags: {diags:?}");
        assert_eq!(stmts.len(), 3);
        let _ = ast;
    }

    #[test]
    fn one_missing_semicolon_is_one_error() {
        // The single most important recovery property: without cascade
        // suppression this produces multiple diagnostics instead of one.
        let src = "let a = 1\nlet b = 2;\nlet c = 3;";
        let (_, _, _, diags) = parse(src);
        let error_count = diags
            .iter()
            .filter(|d| d.severity == ember_diag::Severity::Error)
            .count();
        assert_eq!(error_count, 1, "diags: {diags:?}");
    }

    #[test]
    fn recovery_preserves_surrounding_code() {
        let src = "fn good1() { 1 }\nfn @@@ bad\nfn good2() { 2 }";
        let (ast, _interner, stmts, diags) = parse(src);
        assert!(!diags.is_empty());
        let fn_count = stmts
            .iter()
            .filter(|s| matches!(ast.stmt(**s), Stmt::Fn { .. }))
            .count();
        assert_eq!(
            fn_count, 3,
            "all three fn statements (including the broken one, as an Fn with an Error body) must survive recovery"
        );
    }

    #[test]
    fn unclosed_brace_reports_at_opener() {
        let (_, _, _, diags) = parse_expr_from_str("{ 1;");
        assert_eq!(diags.len(), 1, "diags: {diags:?}");
        assert!(diags[0].message.contains("unclosed"));
        assert_eq!(diags[0].primary_span(), Some(Span::new(0, 1)));
    }

    #[test]
    fn deeply_nested_expression_reports_cleanly_instead_of_overflowing() {
        let mut src = String::new();
        for _ in 0..500 {
            src.push('(');
        }
        src.push('1');
        for _ in 0..500 {
            src.push(')');
        }
        let (_, _, _, diags) = parse_expr_from_str(&src);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("nested too deeply")),
            "diags: {diags:?}"
        );
    }
}
