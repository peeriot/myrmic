use crate::domain::{Key, QuadKey, Scope, TripleKey};
use crate::semantic::{EncodedQuad, EncodedTerm, EncodedTriple};
use anyhow::Context;
use skey::{Decoder, Encoder, KeyError, StoreKey};

pub fn from_triple(
    scope: Scope<'_>,
    encoding: TriEncoding,
    triple: EncodedTriple,
) -> TripleKey<'_> {
    let (a, b, c) = encoding.split(triple);

    scope.triple(encoding).a(a).b(b).c(c)
}

pub fn partial_triple_into_range(
    scope: Scope<'_>,
    subject: Option<&EncodedTerm>,
    predicate: Option<&EncodedTerm>,
    object: Option<&EncodedTerm>,
) -> anyhow::Result<(TriEncoding, Vec<u8>, Vec<u8>)> {
    let encoding =
        TriEncoding::from_partial(subject.is_some(), predicate.is_some(), object.is_some());
    let (a, b, c) = encoding.shuffle(subject, predicate, object);

    let prefix = Key::triple().scope(scope).encoding(encoding);

    let buffer = skey::encode(|encoder| {
        prefix.encode_into(encoder)?;

        if let Some(term) = a {
            skey::StoreKey::encode_into(term, encoder)?;
        }
        if let Some(term) = b {
            skey::StoreKey::encode_into(term, encoder)?;
        }
        if let Some(term) = c {
            skey::StoreKey::encode_into(term, encoder)?;
        }

        Ok(())
    })
    .context("Unable to encode TripleKey")?;

    let (lower, upper) = skey::prefix_to_range(&buffer);
    let upper = upper.context("triple key prefix has no range upper bound")?;

    Ok((encoding, lower, upper))
}

pub fn from_quad(scope: Scope<'_>, encoding: QuadEncoding, quad: EncodedQuad) -> QuadKey<'_> {
    let (a, b, c, d) = encoding.split(quad);

    scope.quad(encoding).a(a).b(b).c(c).d(d)
}

pub fn partial_quad_into_range(
    scope: Scope<'_>,
    subject: Option<&EncodedTerm>,
    predicate: Option<&EncodedTerm>,
    object: Option<&EncodedTerm>,
    graph_name: Option<&EncodedTerm>,
) -> anyhow::Result<(QuadEncoding, Vec<u8>, Vec<u8>)> {
    let encoding = QuadEncoding::from_partial(
        subject.is_some(),
        predicate.is_some(),
        object.is_some(),
        graph_name.is_some(),
    );
    let (a, b, c, d) = encoding.shuffle(subject, predicate, object, graph_name);

    let prefix = Key::quad().scope(scope).encoding(encoding);

    let buffer = skey::encode(|encoder| {
        prefix.encode_into(encoder)?;

        if let Some(term) = a {
            skey::StoreKey::encode_into(term, encoder)?;
        }
        if let Some(term) = b {
            skey::StoreKey::encode_into(term, encoder)?;
        }
        if let Some(term) = c {
            skey::StoreKey::encode_into(term, encoder)?;
        }
        if let Some(term) = d {
            skey::StoreKey::encode_into(term, encoder)?;
        }

        Ok(())
    })
    .context("Unable to encode QuadKey")?;

    let (lower, upper) = skey::prefix_to_range(&buffer);
    let upper = upper.context("quad key prefix has no range upper bound")?;

    Ok((encoding, lower, upper))
}

const MARKER_DSPO: u8 = 1;
const MARKER_DPOS: u8 = 2;
const MARKER_DOSP: u8 = 3;
const MARKER_SPOG: u8 = 4;
const MARKER_POSG: u8 = 5;
const MARKER_OSPG: u8 = 6;
const MARKER_GSPO: u8 = 7;
const MARKER_GPOS: u8 = 8;
const MARKER_GOSP: u8 = 9;

#[derive(Debug, Copy, Clone)]
pub enum TriEncoding {
    Spo,
    Pos,
    Osp,
}

impl<'a> skey::StoreKey<'a> for TriEncoding {
    fn encode_into(&self, encoder: &mut Encoder<'_>) -> Result<(), KeyError> {
        let marker = match self {
            Self::Spo => MARKER_DSPO,
            Self::Pos => MARKER_DPOS,
            Self::Osp => MARKER_DOSP,
        };

        marker.encode_into(encoder)
    }

    fn decode_from(decoder: &mut Decoder<'a>) -> Result<Self, KeyError> {
        Ok(match u8::decode_from(decoder)? {
            MARKER_DSPO => Self::Spo,
            MARKER_DPOS => Self::Pos,
            MARKER_DOSP => Self::Osp,
            marker => {
                anyhow::bail!("Invalid TriEncoding marker: {}", marker)
            }
        })
    }
}

impl TriEncoding {
    pub fn from_partial(subject: bool, predicate: bool, object: bool) -> Self {
        match (subject, predicate, object) {
            (_, false, true) => Self::Osp,
            (false, true, _) => Self::Pos,
            (_, false, false) | (true, true, _) => Self::Spo,
        }
    }

    pub fn split(self, quad: EncodedTriple) -> (EncodedTerm, EncodedTerm, EncodedTerm) {
        let EncodedTriple {
            subject,
            predicate,
            object,
        } = quad;

        self.shuffle(subject, predicate, object)
    }

    /// This function "shuffles" the terms for you.
    /// Transforms everything from (s, p, o)
    pub(crate) fn shuffle<T>(self, a: T, b: T, c: T) -> (T, T, T) {
        match self {
            Self::Spo => (a, b, c),
            Self::Osp => (c, a, b),
            Self::Pos => (b, c, a),
        }
    }

    /// This function "unshuffles" the terms, based on the encoding.
    /// Transforms everything to (s, p, o)
    pub(crate) fn sort<T>(self, a: T, b: T, c: T) -> (T, T, T) {
        match self {
            Self::Spo => (a, b, c),
            Self::Pos => (c, a, b),
            Self::Osp => (b, c, a),
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum QuadEncoding {
    Spog,
    Posg,
    Ospg,
    Gspo,
    Gpos,
    Gosp,
}

impl<'a> skey::StoreKey<'a> for QuadEncoding {
    fn encode_into(&self, encoder: &mut Encoder<'_>) -> Result<(), KeyError> {
        let marker = match self {
            Self::Spog => MARKER_SPOG,
            Self::Posg => MARKER_POSG,
            Self::Ospg => MARKER_OSPG,
            Self::Gspo => MARKER_GSPO,
            Self::Gpos => MARKER_GPOS,
            Self::Gosp => MARKER_GOSP,
        };

        marker.encode_into(encoder)
    }

    fn decode_from(decoder: &mut Decoder<'a>) -> Result<Self, KeyError> {
        Ok(match u8::decode_from(decoder)? {
            MARKER_SPOG => Self::Spog,
            MARKER_POSG => Self::Posg,
            MARKER_OSPG => Self::Ospg,
            MARKER_GSPO => Self::Gspo,
            MARKER_GPOS => Self::Gpos,
            MARKER_GOSP => Self::Gosp,
            marker => {
                anyhow::bail!("Invalid QuadEncoding marker: {}", marker)
            }
        })
    }
}

impl QuadEncoding {
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "the four bools are independent quad components, not a refactorable flag set"
    )]
    pub fn from_partial(subject: bool, predicate: bool, object: bool, graph: bool) -> Self {
        match (TriEncoding::from_partial(subject, predicate, object), graph) {
            (TriEncoding::Spo, true) => Self::Gspo,
            (TriEncoding::Pos, true) => Self::Gpos,
            (TriEncoding::Osp, true) => Self::Gosp,

            (TriEncoding::Spo, false) => Self::Spog,
            (TriEncoding::Pos, false) => Self::Posg,
            (TriEncoding::Osp, false) => Self::Ospg,
        }
    }

    pub fn split(self, quad: EncodedQuad) -> (EncodedTerm, EncodedTerm, EncodedTerm, EncodedTerm) {
        let EncodedQuad {
            subject,
            predicate,
            object,
            graph_name,
        } = quad;

        self.shuffle(subject, predicate, object, graph_name)
    }

    /// This function "shuffles" the terms for you.
    /// Transforms everything from (s, p, o, g)
    pub fn shuffle<T>(self, a: T, b: T, c: T, d: T) -> (T, T, T, T) {
        match self {
            Self::Spog => (a, b, c, d),
            Self::Posg => (b, c, a, d),
            Self::Ospg => (c, a, b, d),
            Self::Gspo => (d, a, b, c),
            Self::Gpos => (d, b, c, a),
            Self::Gosp => (d, c, a, b),
        }
    }

    /// This function "unshuffles" the terms, based on the encoding.
    /// Transforms everything to (s, p, o, g)
    pub fn sort<T>(self, a: T, b: T, c: T, d: T) -> (T, T, T, T) {
        match self {
            Self::Spog => (a, b, c, d),
            Self::Posg => (c, a, b, d),
            Self::Ospg => (b, c, a, d),
            Self::Gspo => (b, c, d, a),
            Self::Gpos => (d, b, c, a),
            Self::Gosp => (c, d, b, a),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{Key, QuadEncoding, QuadKey, Scope, from_quad};
    use crate::semantic::EncodedQuad;
    use skey::StoreKey;

    #[tokio::test(flavor = "current_thread")]
    async fn key_roundtrips() {
        macro_rules! assert_roundtrip {
            ($expr:expr => $ty:ty) => {{
                let value: $ty = $expr;
                let encoded = value
                    .encode()
                    .expect(concat!("Unable to encode value: ", stringify!($expr)));
                let decoded: $ty = skey::StoreKey::decode_from_bytes(encoded.as_slice())
                    .expect(concat!("Unable to decode: ", stringify!($expr)));

                assert_eq!(
                    encoded,
                    decoded
                        .encode()
                        .expect(concat!("Unable to re-encode: ", stringify!($expr)))
                );

                value
            }};
        }

        assert_roundtrip!("hello" => &str);
        assert_roundtrip!(b"hello".as_slice() => &[u8]);

        {
            let right = Key::scope()
                .namespace("public")
                .database("brava")
                .schema("cuteness");
            let right_full = Key::new_scope("public", "brava", "cuteness");

            let encoded = right.encode().unwrap();
            assert_eq!(encoded, right_full.encode().unwrap());
            let left: Scope<'_> = skey::StoreKey::decode_from_bytes(&encoded).unwrap();

            assert_eq!(left.namespace, right.namespace);
            assert_eq!(left.database, right.database);
            assert_eq!(left.schema, right.schema);

            let user = right_full.kv("root");
            let user_full = Key::new_kv("public", "brava", "cuteness", "root");
            assert_eq!(user.encode().unwrap(), user_full.encode().unwrap());

            let graph_name = EncodedQuad::small_text("su", "pr", "ob", "gn").graph_name;
            let graph_qol = right_full.graph_name(graph_name.clone());
            let graph_step = right_full.only_graph_name().name(graph_name);
            assert_eq!(graph_qol.encode().unwrap(), graph_step.encode().unwrap());

            let (lower, upper) = Key::graph_name().scope(right_full).range().unwrap();
            let (expected_lower, expected_upper) = right_full.only_graph_name().range().unwrap();
            assert_eq!((lower, upper), (expected_lower, expected_upper));
        }

        assert_roundtrip!(Key::new_scope("public", "brava", "cuteness") => Scope<'_>);

        let scope = assert_roundtrip!(Scope::default() => Scope<'_>);

        let quad = EncodedQuad::hash_text("su", "pr", "ob", "gn");
        assert_roundtrip!(from_quad(scope, QuadEncoding::Gspo, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Gpos, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Gosp, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Ospg, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Posg, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Spog, quad.clone()) => QuadKey<'_>);

        let quad = EncodedQuad::small_text("su", "pr", "ob", "gn");
        assert_roundtrip!(from_quad(scope, QuadEncoding::Gspo, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Gpos, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Gosp, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Ospg, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Posg, quad.clone()) => QuadKey<'_>);
        assert_roundtrip!(from_quad(scope, QuadEncoding::Spog, quad.clone()) => QuadKey<'_>);
    }
}
