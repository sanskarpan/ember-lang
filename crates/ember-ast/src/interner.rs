use string_interner::{DefaultSymbol, StringInterner};

pub type Symbol = DefaultSymbol;

// `string-interner` 0.17 removed the default generic argument from
// `StringInterner`, so we pin it to the crate's own default backend
// (equivalent to what the old default resolved to).
type Backend = string_interner::DefaultBackend;

#[derive(Default)]
pub struct Interner(StringInterner<Backend>);

impl Interner {
    pub fn new() -> Self {
        Interner(StringInterner::default())
    }

    pub fn intern(&mut self, s: &str) -> Symbol {
        self.0.get_or_intern(s)
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        self.0
            .resolve(sym)
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
