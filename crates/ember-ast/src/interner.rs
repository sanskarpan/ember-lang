use string_interner::{DefaultSymbol, StringInterner};

/// A locally-defined newtype around `string_interner`'s own symbol type
/// (`DefaultSymbol`, aka `SymbolU32`), rather than a plain type alias to it.
///
/// This wrapping exists solely so `Symbol` can implement `serde::Serialize`
/// (see the hand-written impl below, for `ast --json`): Rust's orphan rule
/// forbids implementing a foreign trait (`serde::Serialize`, from the `serde`
/// crate) for a foreign type (`string_interner::SymbolU32`, from the
/// `string-interner` crate) directly, since neither is local to this crate.
/// Wrapping it in a local newtype makes the type local, so the impl is
/// allowed. Every other trait `DefaultSymbol` already derives (`Debug`,
/// `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`) is
/// re-derived here identically, so this is a transparent change for the rest
/// of the workspace, which only ever touches `Symbol` opaquely through
/// `Interner::intern`/`Interner::resolve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Symbol(DefaultSymbol);

impl serde::Serialize for Symbol {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // No `Interner` is available here to resolve this symbol back to its
        // original text, so it serializes as its raw interned id — the same
        // "opaque index, not human text" trade-off already accepted for
        // `Idx<T>` elsewhere in this crate's `--json` support.
        use string_interner::Symbol as _;
        serializer.serialize_u32(self.0.to_usize() as u32)
    }
}

// `string-interner` 0.17 removed the default generic argument from
// `StringInterner`, so we pin it to the crate's own default backend
// (equivalent to what the old default resolved to).
type Backend = string_interner::DefaultBackend;

/// `Clone` is derived so callers (e.g. the REPL's `:type`/`:ast`/`:disasm`
/// meta-commands) can parse/resolve/infer against a throwaway copy without
/// touching the real session `Interner` — safe because `string_interner`'s
/// `StringInterner` clones its backing arena and dedup map wholesale, and
/// because interning is idempotent/append-only: a clone that later diverges
/// from the original (e.g. by interning a name the original never sees)
/// never invalidates any `Symbol` already handed out by the original.
#[derive(Default, Clone)]
pub struct Interner(StringInterner<Backend>);

impl Interner {
    pub fn new() -> Self {
        Interner(StringInterner::default())
    }

    pub fn intern(&mut self, s: &str) -> Symbol {
        Symbol(self.0.get_or_intern(s))
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        self.0
            .resolve(sym.0)
            .expect("symbol not present in this interner")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_text_interns_to_same_symbol() {
        let mut i = Interner::new();
        let a = i.intern("foo");
        let b = i.intern("foo");
        assert_eq!(a, b);
    }

    #[test]
    fn different_text_interns_to_different_symbols() {
        let mut i = Interner::new();
        let a = i.intern("foo");
        let b = i.intern("bar");
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_returns_original_text() {
        let mut i = Interner::new();
        let a = i.intern("hello");
        assert_eq!(i.resolve(a), "hello");
    }
}
