use crate::ty::{AdtId, Ty};
use ember_ast::Symbol;
use rustc_hash::FxHashMap;

pub enum AdtDecl {
    Enum {
        /// Ordered so error messages ("missing variant Circle, Rect")
        /// can report in declaration order — Phase 6's job, but the data
        /// is here now.
        variants: Vec<(Symbol, Vec<Ty>)>,
    },
    Struct {
        field_order: Vec<Symbol>,
        fields: FxHashMap<Symbol, Ty>,
    },
}

#[derive(Default)]
pub struct AdtRegistry {
    by_name: FxHashMap<Symbol, AdtId>,
    /// Maps a variant's own name straight to (owning AdtId, its payload
    /// types) — variant names are unique across the whole program, same as
    /// top-level fn/type/struct names, so this flat map is enough without
    /// needing to know the owning type up front.
    variants: FxHashMap<Symbol, (AdtId, Vec<Ty>)>,
    decls: Vec<AdtDecl>,
    names: Vec<Symbol>,
}

impl AdtRegistry {
    pub fn new() -> Self {
        AdtRegistry::default()
    }

    pub fn register_enum(&mut self, name: Symbol, variants: Vec<(Symbol, Vec<Ty>)>) -> AdtId {
        let id = AdtId(self.decls.len() as u32);
        for (variant_name, payload) in &variants {
            self.variants.insert(*variant_name, (id, payload.clone()));
        }
        self.decls.push(AdtDecl::Enum { variants });
        self.names.push(name);
        self.by_name.insert(name, id);
        id
    }

    pub fn register_struct(&mut self, name: Symbol, fields: Vec<(Symbol, Ty)>) -> AdtId {
        let id = AdtId(self.decls.len() as u32);
        let field_order: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
        self.decls.push(AdtDecl::Struct {
            field_order,
            fields: fields.into_iter().collect(),
        });
        self.names.push(name);
        self.by_name.insert(name, id);
        id
    }

    pub fn id_of(&self, name: Symbol) -> Option<AdtId> {
        self.by_name.get(&name).copied()
    }

    pub fn name_of(&self, id: AdtId) -> Symbol {
        self.names[id.0 as usize]
    }

    pub fn is_struct(&self, id: AdtId) -> bool {
        matches!(self.decls[id.0 as usize], AdtDecl::Struct { .. })
    }

    /// Looks up a variant BY ITS OWN NAME (e.g. `Circle`), returning the
    /// enum type it belongs to and its declared payload types.
    pub fn variant(&self, name: Symbol) -> Option<(AdtId, &[Ty])> {
        self.variants
            .get(&name)
            .map(|(id, payload)| (*id, payload.as_slice()))
    }

    pub fn field_ty(&self, id: AdtId, field: Symbol) -> Option<&Ty> {
        match &self.decls[id.0 as usize] {
            AdtDecl::Struct { fields, .. } => fields.get(&field),
            AdtDecl::Enum { .. } => None,
        }
    }

    /// All field names of a struct, in declaration order, for detecting an
    /// unknown field name in a struct literal and for listing missing ones.
    pub fn struct_fields(&self, id: AdtId) -> impl Iterator<Item = Symbol> + '_ {
        match &self.decls[id.0 as usize] {
            AdtDecl::Struct { field_order, .. } => Some(field_order.iter().copied()),
            AdtDecl::Enum { .. } => None,
        }
        .into_iter()
        .flatten()
    }

    /// Every variant of an enum type, in declaration order, with each one's
    /// payload arity. `None` (empty iterator) for a struct `id`.
    pub fn enum_variants(&self, id: AdtId) -> impl Iterator<Item = (Symbol, usize)> + '_ {
        match &self.decls[id.0 as usize] {
            AdtDecl::Enum { variants } => Some(
                variants
                    .iter()
                    .map(|(name, payload)| (*name, payload.len())),
            ),
            AdtDecl::Struct { .. } => None,
        }
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::Ty;
    use ember_ast::Interner;

    #[test]
    fn register_enum_and_look_up_a_variant() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let point = interner.intern("Point");
        let mut reg = AdtRegistry::new();
        let id = reg.register_enum(shape, vec![(circle, vec![Ty::Float]), (point, vec![])]);
        assert_eq!(reg.id_of(shape), Some(id));
        assert_eq!(reg.name_of(id), shape);
        let (owner, payload) = reg.variant(circle).expect("Circle should be registered");
        assert_eq!(owner, id);
        assert_eq!(payload, &[Ty::Float]);
    }

    #[test]
    fn register_struct_and_look_up_a_field() {
        let mut interner = Interner::new();
        let point = interner.intern("Point");
        let x = interner.intern("x");
        let mut reg = AdtRegistry::new();
        let id = reg.register_struct(point, vec![(x, Ty::Float)]);
        assert_eq!(reg.field_ty(id, x), Some(&Ty::Float));
        assert_eq!(reg.field_ty(id, interner.intern("y")), None);
    }

    #[test]
    fn is_struct_distinguishes_declaration_kinds() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let point = interner.intern("Point");
        let mut reg = AdtRegistry::new();
        let enum_id = reg.register_enum(shape, vec![]);
        let struct_id = reg.register_struct(point, vec![]);
        assert!(!reg.is_struct(enum_id));
        assert!(reg.is_struct(struct_id));
    }

    #[test]
    fn struct_fields_preserves_declaration_order() {
        let mut interner = Interner::new();
        let point = interner.intern("Point");
        let x = interner.intern("x");
        let y = interner.intern("y");
        let z = interner.intern("z");
        let mut reg = AdtRegistry::new();
        let id = reg.register_struct(point, vec![(x, Ty::Float), (y, Ty::Float), (z, Ty::Float)]);
        let order: Vec<_> = reg.struct_fields(id).collect();
        assert_eq!(order, vec![x, y, z]);
    }

    #[test]
    fn enum_variants_lists_every_variant_with_its_arity() {
        let mut interner = Interner::new();
        let shape = interner.intern("Shape");
        let circle = interner.intern("Circle");
        let rect = interner.intern("Rect");
        let point = interner.intern("Point");
        let mut reg = AdtRegistry::new();
        let id = reg.register_enum(
            shape,
            vec![
                (circle, vec![Ty::Float]),
                (rect, vec![Ty::Float, Ty::Float]),
                (point, vec![]),
            ],
        );
        let variants: Vec<_> = reg.enum_variants(id).collect();
        assert_eq!(variants, vec![(circle, 1), (rect, 2), (point, 0)]);
    }
}
