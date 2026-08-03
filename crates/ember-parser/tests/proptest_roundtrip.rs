use ember_ast::print_stmt;
use ember_lexer::TokenKind;
use ember_parser::parse;
use proptest::prelude::*;

/// An identifier that is guaranteed not to collide with a reserved keyword.
/// `[a-z]{1,4}` alone can generate strings like `if`, `let`, `fn`, `for`,
/// `in`, `mut`, etc., which are reserved words, not valid identifiers — a
/// statement like `let if = 1 + 2;` is correctly rejected by the parser.
/// Filtering those out here is what makes the generator actually
/// syntactically-valid-by-construction, matching its intent below.
fn arb_ident() -> impl Strategy<Value = String> {
    "[a-z]{1,4}".prop_filter("must not be a reserved keyword", |s| {
        TokenKind::keyword_from_str(s).is_none()
    })
}

/// A small, syntactically-valid-by-construction generator — rather than
/// generating arbitrary byte strings (which would mostly just fail to
/// parse and prove nothing about round-tripping), this generates small
/// well-formed `let` statements over integer arithmetic, which the parser
/// is guaranteed to accept.
fn arb_let_stmt() -> impl Strategy<Value = String> {
    (arb_ident(), 1i64..1000, arb_ident(), 1i64..1000)
        .prop_map(|(a, n1, b, n2)| format!("let {a} = {n1} + {n2};\nlet {b} = {a} * 2;"))
}

proptest! {
    #[test]
    fn parse_of_print_equals_original_shape(src in arb_let_stmt()) {
        let (ast1, interner1, stmts1, diags1) = parse(&src);
        prop_assert!(diags1.is_empty(), "original didn't parse cleanly: {:?}", diags1);

        let printed: String = stmts1.iter()
            .map(|s| print_stmt(&ast1, &interner1, *s))
            .collect::<Vec<_>>()
            .join("\n");

        let (ast2, interner2, stmts2, diags2) = parse(&printed);
        prop_assert!(diags2.is_empty(), "printed source didn't re-parse cleanly: {:?}\nprinted:\n{}", diags2, printed);
        prop_assert_eq!(stmts1.len(), stmts2.len());

        // Re-printing the re-parsed tree must be byte-identical to the first
        // printing — this is the actual round-trip invariant: print is a
        // fixed point once you're printing already-canonical output.
        let reprinted: String = stmts2.iter()
            .map(|s| print_stmt(&ast2, &interner2, *s))
            .collect::<Vec<_>>()
            .join("\n");
        prop_assert_eq!(printed, reprinted);
    }
}
