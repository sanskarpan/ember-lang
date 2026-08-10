use ember_lexer::lex;
use proptest::prelude::*;

proptest! {
    #[test]
    fn never_panics_on_arbitrary_utf8(s in "\\PC*") {
        let (_tokens, _trivia, _diags) = lex(&s);
    }

    #[test]
    fn spans_tile_arbitrary_source(s in "[a-zA-Z0-9_ \\n\\t+\\-*/=(){}\\[\\];:,.\"]*") {
        let (tokens, _trivia, _diags) = lex(&s);
        let mut cursor = 0u32;
        for t in &tokens {
            prop_assert!(t.span.start >= cursor);
            cursor = t.span.end;
        }
        prop_assert_eq!(cursor as usize, s.len());
    }
}
