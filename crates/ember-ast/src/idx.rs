use std::marker::PhantomData;

pub struct Idx<T> {
    raw: u32,
    _marker: PhantomData<T>,
}

// Every trait below is hand-written instead of derived. `#[derive(...)]` on a
// generic struct adds a `T: Trait` bound even when `T` only appears inside
// `PhantomData<T>` and isn't actually owned — so `#[derive(Clone, Copy,
// PartialEq, Eq, Hash)]` here would silently require e.g. `Expr: Eq + Hash`,
// which `Expr` doesn't implement (it holds an `f64`). An index's identity
// never depends on properties of the thing it points at, so `Idx<T>` must be
// Copy/Eq/Hash for every `T`, unconditionally — exactly what a hand-written
// impl (which only looks at `raw`) gives us and a derive cannot.
impl<T> Clone for Idx<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Idx<T> {}

impl<T> PartialEq for Idx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for Idx<T> {}

impl<T> std::hash::Hash for Idx<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> std::fmt::Debug for Idx<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Idx({})", self.raw)
    }
}

// Hand-written for the same reason as every other impl in this file:
// `#[derive(serde::Serialize)]` would add a `T: Serialize` bound even though
// `T` only appears inside `PhantomData<T>` — an index's serialized form never
// depends on properties of the thing it points at, so `Idx<T>` must be
// `Serialize` for every `T`, unconditionally.
impl<T> serde::Serialize for Idx<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u32(self.raw)
    }
}

impl<T> Idx<T> {
    pub fn new(raw: u32) -> Self {
        Idx {
            raw,
            _marker: PhantomData,
        }
    }

    pub fn index(self) -> usize {
        self.raw as usize
    }
}
