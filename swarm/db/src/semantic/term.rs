use crate::utils::{Either, HashedBytes, SmallString, try_small};
use anyhow::Context;
use educe::Educe;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EncodedTriple {
    pub subject: EncodedTerm,
    pub predicate: EncodedTerm,
    pub object: EncodedTerm,
}

impl EncodedTriple {
    pub fn in_graph(self, graph_name: EncodedTerm) -> EncodedQuad {
        let Self {
            subject,
            predicate,
            object,
        } = self;

        EncodedQuad {
            subject,
            predicate,
            object,
            graph_name,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EncodedQuad {
    pub subject: EncodedTerm,
    pub predicate: EncodedTerm,
    pub object: EncodedTerm,
    pub graph_name: EncodedTerm,
}

impl EncodedQuad {
    #[cfg(test)]
    pub(crate) fn hash_text(
        subject: &str,
        predicate: &str,
        object: &str,
        graph_name: &str,
    ) -> Self {
        Self {
            subject: EncodedTerm::NamedNode(HashedBytes::new(subject)),
            predicate: EncodedTerm::NamedNode(HashedBytes::new(predicate)),
            object: EncodedTerm::NamedNode(HashedBytes::new(object)),
            graph_name: EncodedTerm::NamedNode(HashedBytes::new(graph_name)),
        }
    }

    #[cfg(test)]
    pub(crate) fn small_text(
        subject: &str,
        predicate: &str,
        object: &str,
        graph_name: &str,
    ) -> Self {
        use std::str::FromStr;

        Self {
            subject: EncodedTerm::BlankNodeSmall(
                SmallString::from_str(subject).expect("`subject` must be small."),
            ),
            predicate: EncodedTerm::BlankNodeSmall(
                SmallString::from_str(predicate).expect("`predicate` must be small."),
            ),
            object: EncodedTerm::BlankNodeSmall(
                SmallString::from_str(object).expect("`object` must be small."),
            ),
            graph_name: EncodedTerm::BlankNodeSmall(
                SmallString::from_str(graph_name).expect("`graph_name` must be small."),
            ),
        }
    }
}

/// This is the heart of how we get a Sparql term into our key-value repr.
/// Every possible term is represented in this enum.
#[derive(Clone, Debug, Educe, serde::Serialize, serde::Deserialize)]
#[educe(Hash, PartialEq)]
pub enum EncodedTerm {
    NamedNode(HashedBytes),

    // id
    BlankNodeNumerical([u8; 16]),
    BlankNodeSmall(SmallString),
    BlankNodeBig(HashedBytes),

    // value
    LiteralStringSmall(SmallString),
    LiteralStringBig(HashedBytes),

    // value, language
    LiteralStringLangSmallSmall(SmallString, SmallString),
    LiteralStringLangSmallBig(SmallString, HashedBytes),
    LiteralStringLangBigSmall(HashedBytes, SmallString),
    LiteralStringLangBigBig(HashedBytes, HashedBytes),

    // value, datatype
    LiteralTypedSmall(SmallString, HashedBytes),
    LiteralTypedBig(HashedBytes, HashedBytes),

    // Duration
    LiteralDuration(#[serde(with = "crate::bypass")] oxsdatatypes::Duration),
    LiteralDurationYearMonth(#[serde(with = "crate::bypass")] oxsdatatypes::YearMonthDuration),
    LiteralDurationDayTime(#[serde(with = "crate::bypass")] oxsdatatypes::DayTimeDuration),

    // Literals
    LiteralBoolean(bool),
    LiteralFloat(
        #[educe(Hash(method(hash_float)), PartialEq(method(is_identical_float)))]
        #[serde(with = "crate::bypass")]
        oxsdatatypes::Float,
    ),
    LiteralDouble(
        #[educe(Hash(method(hash_double)), PartialEq(method(is_identical_double)))]
        #[serde(with = "crate::bypass")]
        oxsdatatypes::Double,
    ),
    LiteralInteger(#[serde(with = "crate::bypass")] oxsdatatypes::Integer),
    LiteralDecimal(#[serde(with = "crate::bypass")] oxsdatatypes::Decimal),
    LiteralDateTime(#[serde(with = "crate::bypass")] oxsdatatypes::DateTime),
    LiteralTime(#[serde(with = "crate::bypass")] oxsdatatypes::Time),
    LiteralDate(#[serde(with = "crate::bypass")] oxsdatatypes::Date),
    LiteralGYearMonth(#[serde(with = "crate::bypass")] oxsdatatypes::GYearMonth),
    LiteralGYear(#[serde(with = "crate::bypass")] oxsdatatypes::GYear),
    LiteralGMonthDay(#[serde(with = "crate::bypass")] oxsdatatypes::GMonthDay),
    LiteralGDay(#[serde(with = "crate::bypass")] oxsdatatypes::GDay),
    LiteralGMonth(#[serde(with = "crate::bypass")] oxsdatatypes::GMonth),

    // rdf-star or sparql 1.2 have support for endlessly nested triples.
    Triple(Rc<EncodedTriple>),
}

impl<'a> skey::StoreKey<'a> for EncodedTerm {
    fn encode_into(&self, encoder: &mut skey::Encoder<'_>) -> anyhow::Result<()> {
        <Self as serde::Serialize>::serialize(self, encoder).context("Unable to encode Term")
    }

    fn decode_from(decoder: &mut skey::Decoder<'a>) -> anyhow::Result<Self> {
        <Self as serde::Deserialize>::deserialize(decoder).context("Unable to decode Term")
    }
}

macro_rules! using_be_bytes {
        (
            $(
                $ty:path
            ),* $(,)?
        ) => {

            const _: () = {
                $(
                    impl crate::bypass::Bypass for $ty {
                        fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
                        where
                            S: serde::Serializer,
                        {
                            let bytes = self.to_be_bytes();
                            serde::Serialize::serialize(&bytes, s)
                        }

                        fn deserialize<'de, D>(d: D) -> Result<Self, D::Error>
                        where
                            D: serde::Deserializer<'de> {
                            let bytes = serde::Deserialize::deserialize(d)?;
                            Ok(Self::from_be_bytes(bytes))
                        }
                    }
                )*
            };
        };
    }

using_be_bytes! {
    oxsdatatypes::Integer,
    oxsdatatypes::Duration,
    oxsdatatypes::YearMonthDuration,
    oxsdatatypes::DayTimeDuration,
    oxsdatatypes::Float,
    oxsdatatypes::Double,
    oxsdatatypes::Decimal,
    oxsdatatypes::Time,
    oxsdatatypes::Date,
    oxsdatatypes::DateTime,
    oxsdatatypes::GYearMonth,
    oxsdatatypes::GYear,
    oxsdatatypes::GMonthDay,
    oxsdatatypes::GMonth,
    oxsdatatypes::GDay,
}

impl Eq for EncodedTerm {}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "by-ref signature matches the sibling hash_/is_identical_ helpers"
)]
fn hash_float<H: Hasher>(value: &oxsdatatypes::Float, hasher: &mut H) {
    value.to_be_bytes().hash(hasher);
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "by-ref signature matches the sibling hash_/is_identical_ helpers"
)]
fn hash_double<H: Hasher>(value: &oxsdatatypes::Double, hasher: &mut H) {
    value.to_be_bytes().hash(hasher);
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "by-ref signature matches the sibling hash_/is_identical_ helpers"
)]
fn is_identical_double(left: &oxsdatatypes::Double, right: &oxsdatatypes::Double) -> bool {
    left.is_identical_with(*right)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "by-ref signature matches the sibling hash_/is_identical_ helpers"
)]
fn is_identical_float(left: &oxsdatatypes::Float, right: &oxsdatatypes::Float) -> bool {
    left.is_identical_with(*right)
}

/// These two functions (_transient and _collected) are the way to convert an input type into an encoded form.
///
/// This `transient` form, as the name would imply doesn't keep track of what it converted, and just spits out the encoded form.
/// This is useful if you know the input won't need to be persisted beyond the request.
/// (ie the user performed a Query and after it's returned a result, you can throw this away)
pub(crate) fn encode_term_transient<T, V: EncodeFrom<T>>(value: T) -> V {
    #[expect(
        clippy::expect_used,
        reason = "the hash closure returns Ok unconditionally, so this can never be Err"
    )]
    EncodeFrom::encode_from(value, &mut |input| Ok(HashedBytes::new(input)))
        .expect("[InternalError] no fallible operations")
}

/// These two functions (_transient and _collected) are the way to convert an input type into an encoded form.
///
/// This `collected` form tracks what terms were converted, and returns all inputs that were hashed. (both the original form and the hash form)
/// This is useful when you need to persist such information.
/// (ie, the user inserted something, so we need to record the underlying repr)
pub(crate) fn encode_term_collected<T, V: EncodeFrom<T>>(
    value: T,
) -> (V, Vec<(HashedBytes, String)>) {
    let mut hashed = std::collections::HashMap::new();
    #[expect(
        clippy::expect_used,
        reason = "the hash closure returns Ok unconditionally, so this can never be Err"
    )]
    let value = EncodeFrom::encode_from(value, &mut |input| {
        let hash = HashedBytes::new(input);
        hashed.insert(hash, String::from(input));
        Ok(hash)
    })
    .expect("[InternalError] no fallible operations");

    let hashed = hashed.into_iter().collect();

    (value, hashed)
}

/// This is only used internally within this module, (but technically needs to be exposed due to access rules)
/// This is how we "serialise" the input type to the encoded variant.
pub(crate) trait EncodeFrom<T>: Sized {
    fn encode_from<F>(value: T, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>;
}

impl EncodeFrom<&'_ NamedNode> for EncodedTerm {
    fn encode_from<F>(node: &'_ NamedNode, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        Ok(Self::NamedNode(hash(node.as_str())?))
    }
}

impl EncodeFrom<&'_ BlankNode> for EncodedTerm {
    fn encode_from<F>(node: &'_ BlankNode, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        Ok(if let Some(id) = node.as_ref().unique_id() {
            Self::BlankNodeNumerical(id.to_be_bytes())
        } else {
            try_small(node.as_str(), hash)?.reduce(Self::BlankNodeSmall, Self::BlankNodeBig)
        })
    }
}

impl EncodeFrom<&'_ NamedOrBlankNode> for EncodedTerm {
    fn encode_from<F>(node: &'_ NamedOrBlankNode, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        match node {
            NamedOrBlankNode::NamedNode(node) => Self::encode_from(node, hash),
            NamedOrBlankNode::BlankNode(node) => Self::encode_from(node, hash),
        }
    }
}

impl EncodeFrom<&'_ Term> for EncodedTerm {
    fn encode_from<F>(term: &'_ Term, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        match term {
            Term::NamedNode(node) => Self::encode_from(node, hash),
            Term::BlankNode(node) => Self::encode_from(node, hash),
            Term::Literal(node) => Self::encode_from(node, hash),
            Term::Triple(node) => Self::encode_from(&**node, hash),
        }
    }
}

impl EncodeFrom<&'_ Literal> for EncodedTerm {
    fn encode_from<F>(literal: &'_ Literal, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        let value = literal.value();

        let encoding = match literal.datatype() {
            rdf::LANG_STRING => {
                if let Some(language) = literal.language() {
                    let encoding = match (
                        try_small(value, &mut *hash)?,
                        try_small(language, &mut *hash)?,
                    ) {
                        (Either::Left(value), Either::Left(language)) => {
                            Self::LiteralStringLangSmallSmall(value, language)
                        }
                        (Either::Left(value), Either::Right(language)) => {
                            Self::LiteralStringLangSmallBig(value, language)
                        }
                        (Either::Right(value), Either::Left(language)) => {
                            Self::LiteralStringLangBigSmall(value, language)
                        }
                        (Either::Right(value), Either::Right(language)) => {
                            Self::LiteralStringLangBigBig(value, language)
                        }
                    };
                    Some(encoding)
                } else {
                    None
                }
            }
            xsd::STRING => Some(match try_small(value, &mut *hash)? {
                Either::Left(small) => Self::LiteralStringSmall(small),
                Either::Right(hash) => Self::LiteralStringBig(hash),
            }),
            xsd::BOOLEAN => value.parse().map(Self::LiteralBoolean).ok(),
            xsd::FLOAT => value.parse().map(Self::LiteralFloat).ok(),
            xsd::DOUBLE => value.parse().map(Self::LiteralDouble).ok(),
            xsd::INTEGER
            | xsd::BYTE
            | xsd::SHORT
            | xsd::INT
            | xsd::LONG
            | xsd::UNSIGNED_BYTE
            | xsd::UNSIGNED_SHORT
            | xsd::UNSIGNED_INT
            | xsd::UNSIGNED_LONG
            | xsd::POSITIVE_INTEGER
            | xsd::NON_POSITIVE_INTEGER
            | xsd::NEGATIVE_INTEGER
            | xsd::NON_NEGATIVE_INTEGER => value.parse().map(Self::LiteralInteger).ok(),
            xsd::DECIMAL => value.parse().map(Self::LiteralDecimal).ok(),

            xsd::DATE_TIME | xsd::DATE_TIME_STAMP => value.parse().map(Self::LiteralDateTime).ok(),

            xsd::TIME => value.parse().map(Self::LiteralTime).ok(),
            xsd::DATE => value.parse().map(Self::LiteralDate).ok(),
            xsd::G_YEAR_MONTH => value.parse().map(Self::LiteralGYearMonth).ok(),
            xsd::G_YEAR => value.parse().map(Self::LiteralGYear).ok(),
            xsd::G_MONTH_DAY => value.parse().map(Self::LiteralGMonthDay).ok(),
            xsd::G_DAY => value.parse().map(Self::LiteralGDay).ok(),
            xsd::G_MONTH => value.parse().map(Self::LiteralGMonth).ok(),

            xsd::DURATION => value.parse().map(Self::LiteralDuration).ok(),
            xsd::YEAR_MONTH_DURATION => value.parse().map(Self::LiteralDurationYearMonth).ok(),
            xsd::DAY_TIME_DURATION => value.parse().map(Self::LiteralDurationDayTime).ok(),
            _ => None,
        };

        if let Some(term) = encoding {
            return Ok(term);
        }

        let datatype_hash = hash(literal.datatype().as_str())?;

        Ok(match try_small(value, hash)? {
            Either::Left(small) => Self::LiteralTypedSmall(small, datatype_hash),
            Either::Right(hash) => Self::LiteralTypedBig(hash, datatype_hash),
        })
    }
}

impl EncodeFrom<&'_ Triple> for EncodedTerm {
    fn encode_from<F>(triple: &'_ Triple, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        let triple: EncodedTriple = EncodeFrom::encode_from(triple, hash)?;
        Ok(EncodedTerm::Triple(Rc::new(triple)))
    }
}

impl EncodeFrom<&'_ Triple> for EncodedTriple {
    fn encode_from<F>(triple: &'_ Triple, hash: &mut F) -> anyhow::Result<Self>
    where
        F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
    {
        let Triple {
            subject,
            predicate,
            object,
        } = triple;

        Ok(Self {
            subject: EncodeFrom::encode_from(subject, hash)?,
            predicate: EncodeFrom::encode_from(predicate, hash)?,
            object: EncodeFrom::encode_from(object, hash)?,
        })
    }
}
