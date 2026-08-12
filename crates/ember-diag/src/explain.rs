//! A registry of long-form explanations for every error code the compiler
//! can emit, driving `ember explain <code>`.
//!
//! Each entry is looked up by its stable `E0NNN` code string. The registry
//! is intentionally flat data (no macros, no derive magic) so it stays easy
//! to scan and to keep in sync with the diagnostic call sites that actually
//! produce each code.

/// One `ember explain` entry: a code, a short title, and a longer body
/// (usually a paragraph plus a minimal illustrative example).
pub struct ExplainEntry {
    pub code: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

pub static REGISTRY: &[ExplainEntry] = &[
    // ---------------------------------------------------------------
    // E01xx — Lexer
    // ---------------------------------------------------------------
    ExplainEntry {
        code: "E0101",
        title: "unterminated block comment",
        body: "A `/*` block comment was opened but never closed before the end of the file.\n\nBlock comments in ember nest, so `/* outer /* inner */` still leaves the outer comment open — every `/*` needs its own matching `*/`.\n\nExample:\n\n    /* this comment never ends\n    let x = 1;\n\nAdd the missing `*/` to close the comment.",
    },
    ExplainEntry {
        code: "E0102",
        title: "unexpected character",
        body: "The lexer hit a character that isn't part of any token ember recognizes — not a valid start of an identifier, number, string, operator, or punctuation.\n\nExample:\n\n    let x = 1 @ 2;\n\nHere `@` isn't a valid operator. Remove or replace the offending character. This is often caused by stray punctuation carried over from another language, or an unsupported operator.",
    },
    ExplainEntry {
        code: "E0103",
        title: "unterminated string literal",
        body: "A `\"` string literal was opened but the end of the file was reached before the closing `\"`.\n\nExample:\n\n    let s = \"hello\n\nAdd the missing closing `\"`. If the string is meant to span multiple lines or contain a literal `\"`, escape it with `\\\"`.",
    },
    // ---------------------------------------------------------------
    // E02xx — Parser
    // ---------------------------------------------------------------
    ExplainEntry {
        code: "E0201",
        title: "expression nested too deeply",
        body: "The parser gave up because an expression nested past its maximum recursion depth. This almost always means the source is either pathologically deep (e.g. a machine-generated file with thousands of chained parentheses) or the parser is stuck in a loop from a preceding syntax error, chewing through the rest of the file as one giant malformed expression.\n\nSimplify the expression (break it into intermediate `let` bindings) or check for an earlier unbalanced delimiter that could be causing the parser to over-nest.",
    },
    ExplainEntry {
        code: "E0202",
        title: "expected an expression",
        body: "The parser needed an expression at this position — the start of a statement, an operand, an argument, and so on — but found a token that can't start one.\n\nExample:\n\n    let x = ;\n\nHere `;` can't start an expression. Supply a value, or remove the stray token.",
    },
    ExplainEntry {
        code: "E0203",
        title: "invalid assignment target",
        body: "The left-hand side of an `=` isn't something that can be assigned to. Only variables, field accesses (`obj.field`), and index expressions (`list[i]`) are valid assignment targets.\n\nExample:\n\n    1 + 2 = 3;\n\n`1 + 2` isn't a place a value can be stored. Assign to a variable, field, or index expression instead.",
    },
    ExplainEntry {
        code: "E0204",
        title: "unclosed delimiter",
        body: "A `(`, `[`, `{`, `<`, or similar opening delimiter was never matched by its closing counterpart before the parser ran out of tokens (or hit something that couldn't continue the construct). This code covers call argument lists, index expressions, blocks, struct/record literals, generic argument lists, tuples, list literals, function parameter lists, and match arms alike — they all report at the *opening* delimiter, not at end-of-file, since that's where the fix actually belongs.\n\nExample:\n\n    fn f(x: Int {\n        x\n    }\n\nThe `(` after `f` is never closed with `)`. Add the missing closing delimiter.",
    },
    ExplainEntry {
        code: "E0205",
        title: "expected an identifier",
        body: "The parser needed a plain identifier at this position — a field name after `.`, a field name in a struct/record literal or pattern, or a parameter name — but found something else.\n\nExample:\n\n    point.1\n\n`.` must be followed by a field name, not a number. Use a valid identifier here.",
    },
    ExplainEntry {
        code: "E0206",
        title: "expected a specific token",
        body: "The parser required one particular token at this position — such as `=` after a `let` name, `;` to terminate a statement, `(` after a function name, `{` after a struct name, `:` after a field name, `in` after a `for` binding, `->` in a function type, `>` to close generic arguments, `=>` after a match pattern, or `=` after a type name — and found a different one.\n\nThe diagnostic's own message and the `found ...` label say exactly which token was expected and which one was actually there; insert the missing token or fix the typo.",
    },
    ExplainEntry {
        code: "E0207",
        title: "expected `;` after expression",
        body: "An expression statement (one not ending in `}`, like an `if`, block, or `match`) must be terminated by `;`.\n\nExample:\n\n    let x = 1\n    let y = 2;\n\nThe first line is missing its `;`, so the parser tries to continue the statement into the next line. Add the missing semicolon after `1`.",
    },
    ExplainEntry {
        code: "E0208",
        title: "expected `{` to start a block",
        body: "A construct that requires a brace-delimited block — a function body, `while`/`for`/`loop` body, or an `if`/`else` branch — wasn't followed by `{`.\n\nExample:\n\n    fn f() x\n\nA function body must be a `{ ... }` block, even for a single expression: `fn f() { x }`.",
    },
    ExplainEntry {
        code: "E0209",
        title: "expected a type",
        body: "A type annotation position (after `:`, in a generic argument list, in a function parameter's or return's type, etc.) needs a type expression — a name like `Int`, a list type `[T]`, a generic type `Option<T>`, or a function type `(T) -> U` — but found something that isn't one.\n\nExample:\n\n    let x: 5 = 5;\n\n`5` is a value, not a type. Use a type name such as `Int` instead.",
    },
    ExplainEntry {
        code: "E0210",
        title: "expected a pattern",
        body: "A pattern position — a `let` binding's left-hand side, a function parameter, or a `match` arm — needs a pattern (a binding name, literal, constructor, list, tuple, or wildcard `_`) but found something that isn't one.\n\nExample:\n\n    match x {\n        + => 1,\n    }\n\n`+` can't start a pattern. Use a valid pattern such as an identifier, literal, or `_`.",
    },
    // ---------------------------------------------------------------
    // E03xx — Resolver
    // ---------------------------------------------------------------
    ExplainEntry {
        code: "E0301",
        title: "cannot use a name in its own initializer",
        body: "A `let` binding's own name was referenced inside its own initializer expression, before the binding exists. ember declares a `let` name before evaluating its initializer (so shadowing an outer name of the same kind works predictably), but the new binding isn't considered *initialized* until after the initializer runs — so referring to it during that window is an error rather than silently seeing an outer binding or an uninitialized value.\n\nExample:\n\n    let x = x + 1;\n\nIf you meant to reference an outer `x`, rename this binding. If you meant to build a value incrementally, initialize it first and update it afterward.",
    },
    ExplainEntry {
        code: "E0302",
        title: "undeclared name",
        body: "A name was referenced that isn't declared anywhere visible from this point in the program — not as a local, a function parameter, an upvalue, or a top-level `let`/`fn`/`type`/`struct`.\n\nExample:\n\n    print(totally_undefined_name);\n\nCheck for typos (the diagnostic suggests the closest declared name it can find, if any) or declare the name with `let` before using it.",
    },
    ExplainEntry {
        code: "E0303",
        title: "unused variable",
        body: "A `let` binding, function parameter, or `for` loop binding was never read after being declared. This is a warning, not an error — the program still runs — but it usually points at dead code or a typo where a different variable was meant to be used.\n\nExample:\n\n    fn f() {\n        let result = compute();\n        42\n    }\n\n`result` is computed but never used. If this is intentional (e.g. a value kept only for a side effect), prefix the name with an underscore (`_result`) to silence the warning.",
    },
    ExplainEntry {
        code: "E0304",
        title: "unreachable code",
        body: "Code appears after a `return`, `break`, or `continue` within the same block, so it can never execute. This is a warning, not an error.\n\nExample:\n\n    fn f() {\n        return 1;\n        print(\"never runs\");\n    }\n\nRemove the unreachable statement, or move it before the `return`/`break`/`continue` if it was meant to run.",
    },
    ExplainEntry {
        code: "E0305",
        title: "cannot assign to immutable variable",
        body: "An assignment (`=`) targeted a variable that was declared with plain `let`, not `let mut`. ember bindings are immutable by default; only `let mut`-declared variables can be reassigned after their initial value.\n\nExample:\n\n    let x = 1;\n    x = 2;\n\nChange the declaration to `let mut x = 1;` if `x` is meant to be reassigned, or introduce a new binding instead.",
    },
    ExplainEntry {
        code: "E0306",
        title: "cannot assign to immutable captured variable",
        body: "An assignment targeted a variable that a closure captured from an enclosing scope, but the outer binding wasn't declared `mut`. Capturing a variable by reference doesn't itself grant permission to mutate it — the outer `let` still has to opt in with `mut`.\n\nExample:\n\n    let counter = 0;\n    let bump = || { counter = counter + 1; };\n\nChange the outer declaration to `let mut counter = 0;` so the closure is allowed to reassign it.",
    },
    // ---------------------------------------------------------------
    // E04xx — Type inference
    // ---------------------------------------------------------------
    ExplainEntry {
        code: "E0401",
        title: "undeclared name",
        body: "Type inference encountered a variable reference with no entry in its type environment. In a well-formed program the resolver (E0302) would already have caught this earlier in the pipeline, so this code mainly shows up when a name resolves structurally (e.g. it exists in the resolver's scopes) but wasn't ever given a type — most commonly from a bug in an earlier compiler stage rather than a typo in ember source. If you see this on ordinary source code, treat it the same as E0302: check for typos or a missing declaration.",
    },
    ExplainEntry {
        code: "E0402",
        title: "wrong number of arguments",
        body: "A function was called with a different number of arguments than its type expects. ember doesn't support variadic functions or default parameter values, so every call must supply exactly the declared number of arguments.\n\nExample:\n\n    fn add(a: Int, b: Int) -> Int { a + b }\n    add(1);\n\n`add` takes 2 arguments but only 1 was supplied. Add the missing argument, or check whether a different function was intended.",
    },
    ExplainEntry {
        code: "E0403",
        title: "not callable",
        body: "An expression was called like a function (`expr(...)`) but its type isn't a function type — for example an `Int`, a `String`, or a struct value.\n\nExample:\n\n    let x = 5;\n    x(1, 2);\n\n`x` is an `Int`, not a function, so it can't be called. Check for a missing function definition or a variable name that shadowed the function you meant to call.",
    },
    ExplainEntry {
        code: "E0404",
        title: "unknown type",
        body: "A type annotation or struct-literal name referred to a type name that isn't `Int`, `Float`, `Bool`, `String`, `Unit`, a list type, or any `type`/`struct` declared in the program.\n\nExample:\n\n    let x: Integer = 5;\n\nember's built-in integer type is spelled `Int`, not `Integer`. Check for a typo, or make sure the `type`/`struct` you meant to reference is actually declared (and declared before use, or anywhere at top level — top-level type declarations are visible throughout the file).",
    },
    ExplainEntry {
        code: "E0405",
        title: "type does not take type arguments",
        body: "A type name was written with angle-bracket type arguments (like `Foo<Int>`), but that type isn't generic. Currently only the built-in `List<T>` form (equivalently `[T]`) accepts a type argument — user-declared `type`/`struct` declarations in this phase of ember have no generic parameters.\n\nExample:\n\n    struct Point { x: Int, y: Int }\n    let p: Point<Int> = Point { x: 1, y: 2 };\n\n`Point` isn't generic — drop the `<Int>`.",
    },
    ExplainEntry {
        code: "E0406",
        title: "not a struct",
        body: "Struct-literal syntax (`Name { field: value, ... }`) was used with a name that refers to a declared type, but that type is a tagged-union (`type ... = A | B`) variant tag, not a `struct`. Struct-literal syntax only applies to `struct` declarations.\n\nExample:\n\n    type Shape = Circle(Float) | Square(Float);\n    let s = Circle { radius: 1.0 };\n\n`Circle` is a union variant with a positional payload, constructed as `Circle(1.0)`, not with struct-literal braces.",
    },
    ExplainEntry {
        code: "E0407",
        title: "unknown field",
        body: "A struct literal or field access (`.field`) referenced a field name that isn't declared on the struct's type.\n\nExample:\n\n    struct Point { x: Int, y: Int }\n    let p = Point { x: 1, y: 2 };\n    p.z\n\n`Point` has no field `z`. Check for a typo, or add the field to the `struct` declaration if it's meant to exist.",
    },
    ExplainEntry {
        code: "E0408",
        title: "missing field in struct literal",
        body: "A struct literal didn't provide a value for one of its struct's declared fields. Every field must be given a value; ember has no default field values.\n\nExample:\n\n    struct Point { x: Int, y: Int }\n    let p = Point { x: 1 };\n\n`Point` also requires a `y` field. Add `y: <value>` to the literal.",
    },
    ExplainEntry {
        code: "E0409",
        title: "cannot infer the type of this field access",
        body: "A `.field` access was type-checked before enough was known about the base expression's type to know which struct's fields to look up — its type was still an unresolved type variable even after inference finished. ember doesn't do structural (duck-typed) field access across unrelated struct declarations, so the base's concrete struct type has to be pinned down some other way.\n\nExample:\n\n    fn get_x(p) { p.x }\n\n`p`'s type is never otherwise constrained, so its struct type can't be inferred just from `.x`. Add an explicit type annotation, e.g. `fn get_x(p: Point) { p.x }`.",
    },
    ExplainEntry {
        code: "E0410",
        title: "type has no fields",
        body: "A `.field` access was used on a value whose type isn't a struct at all — an `Int`, `String`, `List`, function, or union-variant value, none of which have named fields.\n\nExample:\n\n    let x = 5;\n    x.field\n\n`Int` has no fields. Check whether the wrong variable was used, or whether the base expression's type is what you expected.",
    },
    ExplainEntry {
        code: "E0411",
        title: "constructor pattern arity mismatch",
        body: "A constructor pattern in a `match` arm (like `Circle(r)`) supplied a different number of sub-patterns than that variant's declared payload.\n\nExample:\n\n    type Shape = Circle(Float) | Rect(Float, Float);\n    match s {\n        Rect(w) => w,\n        _ => 0.0,\n    }\n\n`Rect` carries two payload values, but the pattern `Rect(w)` only binds one. Add the missing sub-pattern (e.g. `Rect(w, h)`), or use `_` for any part you don't need.",
    },
    ExplainEntry {
        code: "E0412",
        title: "unknown constructor",
        body: "A constructor pattern (`Name(...)`) in a `match` arm referenced a name that isn't a declared union-type variant.\n\nExample:\n\n    match s {\n        Triangle(a, b, c) => a,\n        _ => 0.0,\n    }\n\nIf `Triangle` isn't one of the `type`'s declared variants, this pattern can never match anything. Check for a typo, or add the missing variant to the `type` declaration.",
    },
    ExplainEntry {
        code: "E0413",
        title: "infinite type",
        body: "Unification tried to make a type variable equal to a type that contains that very same variable — the classic infinite/recursive-type occurs-check failure. Without this check, the compiler could construct a type that's infinitely large (e.g. `a = [a]`), which nothing in the type system can represent or make sense of.\n\nThis usually comes from a function whose inferred return type ends up depending on calling itself with its own, still-unresolved type — for example a recursive function missing a type annotation that would otherwise pin its parameter/return types down. Adding explicit type annotations to the function in question is the most reliable fix.",
    },
    ExplainEntry {
        code: "E0414",
        title: "type mismatch",
        body: "Two types that were expected to be the same turned out not to unify. This single code covers many different situations that all reduce to the same underlying problem: `if`/`else` branches with different types, a call argument whose type doesn't match the parameter's declared type, the two operands of a binary operator disagreeing, a value's type not matching its explicit annotation, `match` arms producing different types, a function's actual return type not matching its declared one, list literal elements of inconsistent types, a `while`/`if` condition that isn't `Bool`, and index/assignment type mismatches.\n\nExample:\n\n    let x: Int = \"hello\";\n\n`\"hello\"` is a `String`, but `x` was annotated `Int`. The diagnostic's labels point at exactly which two expressions disagreed and what each one's type was inferred to be; reconcile them by fixing whichever side has the wrong type, or by adding an explicit conversion.",
    },
    // ---------------------------------------------------------------
    // E05xx — Exhaustiveness
    // ---------------------------------------------------------------
    ExplainEntry {
        code: "E0501",
        title: "unreachable pattern",
        body: "A `match` arm's pattern can never match anything, because every value it could match is already fully covered by earlier arms. This is a warning, not an error.\n\nExample:\n\n    match x {\n        _ => 1,\n        0 => 2,\n    }\n\nThe first arm (`_`) already matches everything, so the `0` arm below it is dead code. Reorder the arms so more specific patterns come before the catch-all, or remove the unreachable arm.",
    },
    ExplainEntry {
        code: "E0502",
        title: "non-exhaustive patterns",
        body: "A `match` expression doesn't cover every possible value of its scrutinee's type — there's at least one value that none of the arms would match, which would leave the match with nothing to run at runtime.\n\nExample:\n\n    type Shape = Circle(Float) | Square(Float);\n    match s {\n        Circle(r) => r,\n    }\n\nThe `Square` case is never handled. Add an arm for the missing case(s) (the diagnostic's note lists a representative example of what's missing), or add a catch-all `_ => ...` arm.",
    },
];

/// Looks up the explanation entry for a diagnostic code, e.g. `"E0301"`.
pub fn lookup(code: &str) -> Option<&'static ExplainEntry> {
    REGISTRY.iter().find(|e| e.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Diagnostic::error`/`::warning` call site across the
    /// diagnostic-producing crates tags itself with a `"E0NNN"` string
    /// literal (either directly via `.with_code("E0NNN")`, or — in the
    /// lexer's case — as a `code` argument literal threaded through a small
    /// helper). This test re-derives the ground-truth set of codes by
    /// scanning those same source files for `"E0NNN"` literals via
    /// `include_str!`, rather than trusting a hand-maintained list, so it
    /// actually fails if a future diagnostic call site introduces a new
    /// code without a matching registry entry (or if a code here becomes
    /// stale because its call site was removed).
    const SOURCE_FILES: &[(&str, &str)] = &[
        (
            "ember-lexer/src/lex.rs",
            include_str!("../../ember-lexer/src/lex.rs"),
        ),
        (
            "ember-parser/src/parser.rs",
            include_str!("../../ember-parser/src/parser.rs"),
        ),
        (
            "ember-resolve/src/resolver.rs",
            include_str!("../../ember-resolve/src/resolver.rs"),
        ),
        (
            "ember-types/src/infer.rs",
            include_str!("../../ember-types/src/infer.rs"),
        ),
        (
            "ember-types/src/unify.rs",
            include_str!("../../ember-types/src/unify.rs"),
        ),
        (
            "ember-types/src/exhaustive.rs",
            include_str!("../../ember-types/src/exhaustive.rs"),
        ),
    ];

    /// Extracts every `"E0NNN"` (exactly 4 digits) string literal appearing
    /// in `src`, without pulling in a regex dependency just for this test.
    fn extract_codes(src: &str) -> Vec<String> {
        let mut codes = Vec::new();
        let bytes = src.as_bytes();
        let mut i = 0;
        while i + 6 <= bytes.len() {
            if bytes[i] == b'"'
                && bytes[i + 1] == b'E'
                && bytes[i + 2] == b'0'
                && bytes[i + 3].is_ascii_digit()
                && bytes[i + 4].is_ascii_digit()
                && bytes[i + 5].is_ascii_digit()
                && i + 6 < bytes.len()
                && bytes[i + 6] == b'"'
            {
                codes.push(src[i + 1..i + 6].to_string());
                i += 7;
            } else {
                i += 1;
            }
        }
        codes
    }

    #[test]
    fn every_real_diagnostic_code_has_a_registry_entry() {
        let mut missing = Vec::new();
        let mut found_any = false;
        for (file, src) in SOURCE_FILES {
            for code in extract_codes(src) {
                found_any = true;
                if lookup(&code).is_none() {
                    missing.push(format!("{code} (used in {file})"));
                }
            }
        }
        assert!(
            found_any,
            "extract_codes found zero E0NNN literals across the scanned source files — \
             the include_str! paths are probably wrong, which would make this test \
             vacuously pass instead of actually checking anything"
        );
        assert!(
            missing.is_empty(),
            "these diagnostic codes are used in real compiler source but have no \
             explain::REGISTRY entry: {missing:#?}"
        );
    }

    /// The inverse check: every registry entry should correspond to a code
    /// that's actually used somewhere, so the registry doesn't silently
    /// accumulate stale entries for codes nothing emits anymore.
    #[test]
    fn every_registry_entry_corresponds_to_a_real_call_site() {
        let mut used_codes = std::collections::HashSet::new();
        for (_, src) in SOURCE_FILES {
            for code in extract_codes(src) {
                used_codes.insert(code);
            }
        }
        let stale: Vec<&str> = REGISTRY
            .iter()
            .map(|e| e.code)
            .filter(|c| !used_codes.contains(*c))
            .collect();
        assert!(
            stale.is_empty(),
            "these explain::REGISTRY entries have no corresponding diagnostic call site: {stale:?}"
        );
    }

    #[test]
    fn lookup_finds_a_known_code() {
        let entry = lookup("E0301").expect("E0301 should be registered");
        assert_eq!(entry.code, "E0301");
        assert!(!entry.title.is_empty());
        assert!(!entry.body.is_empty());
    }

    #[test]
    fn lookup_returns_none_for_an_unknown_code() {
        assert!(lookup("E9999").is_none());
    }

    #[test]
    fn every_entry_has_a_well_formed_code_title_and_body() {
        for entry in REGISTRY {
            assert!(
                entry.code.starts_with('E') && entry.code.len() == 5,
                "malformed code: {}",
                entry.code
            );
            assert!(!entry.title.is_empty(), "{} has an empty title", entry.code);
            assert!(!entry.body.is_empty(), "{} has an empty body", entry.code);
        }
    }

    #[test]
    fn registry_has_no_duplicate_codes() {
        let mut seen = std::collections::HashSet::new();
        for entry in REGISTRY {
            assert!(
                seen.insert(entry.code),
                "duplicate registry entry for {}",
                entry.code
            );
        }
    }
}
