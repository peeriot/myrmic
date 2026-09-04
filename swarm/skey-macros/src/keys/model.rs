use proc_macro2::Ident;
use proc_macro2::Span;

#[derive(Clone)]
pub enum SegmentSpec {
    Literal(syn::LitStr),
    Field { name: Ident, kind: SegmentKind },
    Ref(Ident),
}

#[derive(Clone)]
pub enum SegmentKind {
    Str,
    Bytes,
    Type { ty: Box<syn::Type>, repr: String },
}

impl PartialEq for SegmentKind {
    fn eq(&self, other: &Self) -> bool {
        use SegmentKind::{Bytes, Str, Type};

        match (self, other) {
            (Str, Str) | (Bytes, Bytes) => true,
            (Type { repr: a, .. }, Type { repr: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Eq for SegmentKind {}

#[derive(Clone)]
pub enum Segment {
    Literal(syn::LitStr),
    Field { name: Ident, kind: SegmentKind },
}

impl PartialEq for Segment {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Segment::Literal(a), Segment::Literal(b)) => a.value() == b.value(),
            (
                Segment::Field {
                    name: a_name,
                    kind: a_kind,
                    ..
                },
                Segment::Field {
                    name: b_name,
                    kind: b_kind,
                    ..
                },
            ) => a_name == b_name && a_kind == b_kind,
            _ => false,
        }
    }
}

impl Eq for Segment {}

impl Segment {
    pub fn span(&self) -> Span {
        match self {
            Segment::Literal(lit) => lit.span(),
            Segment::Field { name, .. } => name.span(),
        }
    }
}

pub struct TreeDsl {
    pub items: Vec<TreeItem>,
}

pub enum TreeItem {
    Alias(AliasDef),
    Key(KeyDef),
    Type(TypeDef),
}

pub struct AliasDef {
    pub name: Ident,
    pub segments: Vec<SegmentSpec>,
}

pub struct KeyDef {
    pub name: Ident,
    pub docs: Vec<syn::Attribute>,
    pub segments: Vec<SegmentSpec>,
}

pub struct TypeDef {
    pub name: Ident,
    pub ty: syn::Type,
    pub no_copy: bool,
}

pub struct KeyModel {
    pub name: Ident,
    pub docs: Vec<syn::Attribute>,
    pub no_copy: bool,
    pub segments: Vec<Segment>,
    pub fields: Vec<(Ident, SegmentKind)>,
    pub has_borrowed_fields: bool,
}

#[derive(Default)]
pub struct KeyAttrs {
    pub docs: Vec<syn::Attribute>,
    pub no_copy: bool,
}

pub struct AliasModel {
    pub name: Ident,
    pub fields: Vec<(Ident, SegmentKind)>,
}
