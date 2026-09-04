use crate::domain::{
    Key, QuadEncoding, Scope, TriEncoding, from_quad, from_triple, partial_quad_into_range,
    partial_triple_into_range,
};
use crate::store::fjall::Transaction;
use crate::utils::HashedBytes;
use std::rc::Rc;

use anyhow::Context as _;

use crate::semantic::{
    EncodedTerm, EncodedTriple, convert_graph_name, convert_triple, encode_term_collected,
    encode_term_transient, fill_ground_quad_pattern, fill_quad_pattern,
};
use oxrdf::{BlankNode, Literal, NamedNode, Term, Triple};
use oxsdatatypes::Boolean;
use rustc_hash::FxHashMap;
use skey::StoreKey;
use spareval::{ExpressionTerm, ExpressionTriple, InternalQuad, QuerySolution};
use spargebra::term::{GroundQuadPattern, QuadPattern};

fn sem_insert_hash_lookup<M>(
    tx: &mut Transaction<M>,
    hash: HashedBytes,
    value: &str,
) -> anyhow::Result<()> {
    let hash = hash.to_be_bytes();
    let key = Key::new_sem_hash(hash.as_slice());

    tx.put_if_absent(&key, value.as_bytes())?;

    Ok(())
}

pub(super) fn sem_insert_graph_name<M>(
    tx: &mut Transaction<M>,
    scope: Scope<'_>,
    named_node: &NamedNode,
) -> anyhow::Result<HashedBytes> {
    let name = named_node.as_str();

    let hash = HashedBytes::new(name);

    let node = EncodedTerm::NamedNode(hash);
    let key = scope.graph_name(node);

    tx.put_if_absent(&key, name.as_bytes())?;

    sem_insert_hash_lookup(&mut *tx, hash, name)?;

    Ok(hash)
}

pub(super) fn sem_eval<'tx, M>(
    tx: &'tx mut Transaction<M>,
    scope: Scope<'_>,
    query: crate::semantic::Query,
) -> anyhow::Result<spareval::QueryResults<'tx>> {
    let dv = DataView::new(tx, scope)?;

    let eval = spareval::QueryEvaluator::new();
    let query = query.into();

    eval.prepare(&query)
        .execute(dv)
        .context("unable to eval query")
}

pub(super) fn sem_delete_insert<M>(
    tx: &mut Transaction<M>,
    scope: Scope<'_>,
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    solutions: &[QuerySolution],
) -> anyhow::Result<()> {
    let mut blanks = FxHashMap::default();

    for solution in solutions {
        for delete in delete {
            if let Some(quad) = fill_ground_quad_pattern(delete, solution) {
                let graph_name = quad.graph_name;
                let triple = Triple::new(quad.subject, quad.predicate, quad.object);

                let triple: EncodedTriple = encode_term_transient(&triple);

                sem_delete_quad(&mut *tx, scope, graph_name, triple)?;
            }
        }

        for insert in insert {
            if let Some(quad) = fill_quad_pattern(insert, solution, &mut blanks) {
                let graph_name = quad.graph_name;
                let triple = Triple::new(quad.subject, quad.predicate, quad.object);

                let (triple, hashed) = encode_term_collected(&triple);
                for (hash, input) in hashed {
                    sem_insert_hash_lookup(&mut *tx, hash, &input)?;
                }

                sem_insert_quad(&mut *tx, scope, graph_name, triple)?;
            }
        }

        blanks.clear();
    }

    Ok(())
}

pub(super) fn sem_insert_data<M>(
    tx: &mut Transaction<M>,
    scope: Scope<'_>,
    data: Vec<spargebra::term::Quad>,
) -> anyhow::Result<()> {
    let mut blanks = FxHashMap::default();

    for quad in data {
        let graph_name = convert_graph_name(quad.graph_name);

        let triple = Triple::new(quad.subject, quad.predicate, quad.object);
        let triple = convert_triple(triple, &mut blanks);

        let (triple, hashed) = encode_term_collected(&triple);
        for (hash, input) in hashed {
            sem_insert_hash_lookup(&mut *tx, hash, &input)?;
        }

        sem_insert_quad(&mut *tx, scope, graph_name, triple)?;
    }

    Ok(())
}

pub(super) fn sem_delete_data<M>(
    tx: &mut Transaction<M>,
    scope: Scope<'_>,
    data: Vec<spargebra::term::GroundQuad>,
) -> anyhow::Result<()> {
    for quad in data {
        let graph_name = convert_graph_name(quad.graph_name);
        let triple = Triple::new(quad.subject, quad.predicate, quad.object);

        let triple = encode_term_transient(&triple);

        sem_delete_quad(&mut *tx, scope, graph_name, triple)?;
    }

    Ok(())
}

fn sem_delete_quad<M>(
    tx: &mut Transaction<M>,
    scope: Scope<'_>,
    graph_name: oxrdf::GraphName,
    triple: EncodedTriple,
) -> anyhow::Result<()> {
    let term = match graph_name {
        oxrdf::GraphName::DefaultGraph => {
            tx.erase(&from_triple(scope, TriEncoding::Spo, triple.clone()))?;
            tx.erase(&from_triple(scope, TriEncoding::Pos, triple.clone()))?;
            tx.erase(&from_triple(scope, TriEncoding::Osp, triple.clone()))?;

            // No common code for the default graph.
            return Ok(());
        }
        oxrdf::GraphName::BlankNode(node) => encode_term_transient(&node),
        oxrdf::GraphName::NamedNode(node) => encode_term_transient(&node),
    };

    let quad = triple.in_graph(term);

    tx.erase(&from_quad(scope, QuadEncoding::Spog, quad.clone()))?;
    tx.erase(&from_quad(scope, QuadEncoding::Posg, quad.clone()))?;
    tx.erase(&from_quad(scope, QuadEncoding::Ospg, quad.clone()))?;
    tx.erase(&from_quad(scope, QuadEncoding::Gspo, quad.clone()))?;
    tx.erase(&from_quad(scope, QuadEncoding::Gpos, quad.clone()))?;
    tx.erase(&from_quad(scope, QuadEncoding::Gosp, quad.clone()))?;

    Ok(())
}

fn sem_insert_quad<M>(
    tx: &mut Transaction<M>,
    scope: Scope<'_>,
    graph_name: oxrdf::GraphName,
    triple: EncodedTriple,
) -> anyhow::Result<()> {
    macro_rules! insert {
        (
            $first:expr,
            $(
                $encoding:expr
            ),* $(,)?
        ) => {{
            if tx.put_if_absent(&$first, b"")? {
                $(
                    tx.put(&$encoding, b"")
                        .context("Unable to insert key")?;
                )*
            }
        }};
    }

    let term = match graph_name {
        oxrdf::GraphName::DefaultGraph => {
            insert![
                from_triple(scope, TriEncoding::Spo, triple.clone()),
                from_triple(scope, TriEncoding::Pos, triple.clone()),
                from_triple(scope, TriEncoding::Osp, triple.clone()),
            ];

            // No common code for the default graph.
            return Ok(());
        }
        oxrdf::GraphName::BlankNode(node) => {
            let (term, collected): (EncodedTerm, _) = encode_term_collected(&node);
            for (hash, input) in collected {
                sem_insert_hash_lookup(&mut *tx, hash, &input)?;
            }

            term
        }
        oxrdf::GraphName::NamedNode(named_node) => {
            let hash = sem_insert_graph_name(tx, scope, &named_node)?;
            EncodedTerm::NamedNode(hash)
        }
    };

    let quad = triple.in_graph(term);

    insert![
        from_quad(scope, QuadEncoding::Spog, quad.clone()),
        from_quad(scope, QuadEncoding::Posg, quad.clone()),
        from_quad(scope, QuadEncoding::Ospg, quad.clone()),
        from_quad(scope, QuadEncoding::Gspo, quad.clone()),
        from_quad(scope, QuadEncoding::Gpos, quad.clone()),
        from_quad(scope, QuadEncoding::Gosp, quad.clone()),
    ];

    Ok(())
}

macro_rules! tri {
    ($expr:expr) => {{
        let expr = $expr;
        match expr {
            Ok(value) => value,
            Err(err) => {
                return std::boxed::Box::new(std::iter::once(Err(err.into())))
                    as Box<dyn Iterator<Item = _>>;
            }
        }
    }};
}

type InternalTerm<'a, T> = <T as spareval::QueryableDataset<'a>>::InternalTerm;

type QuadIter<'a, T> =
    Box<dyn Iterator<Item = Result<InternalQuad<InternalTerm<'a, T>>, SemanticError>>>;

pub struct DataView<'tx, M> {
    scope: Vec<u8>,
    tx: &'tx mut Transaction<M>,
}

impl<'tx, M> DataView<'tx, M> {
    pub fn new(tx: &'tx mut Transaction<M>, scope: Scope<'_>) -> anyhow::Result<Self> {
        let encoded = scope.encode().context("Unable to encode scope")?;

        let decoded: Scope<'_> =
            StoreKey::decode_from_bytes(encoded.as_slice()).context("Unable to decode scope")?;

        let same = scope.namespace == decoded.namespace
            && scope.database == decoded.database
            && scope.schema == decoded.schema;

        if !same {
            anyhow::bail!("Unable to correctly decode scope")
        }

        Ok(Self { scope: encoded, tx })
    }

    fn decode_term(&self, term: EncodedTerm) -> anyhow::Result<Term> {
        macro_rules! resolve {
            ($hash:expr) => {{
                let hash = $hash;
                let hash = Key::new_sem_hash(hash.as_slice());
                let data = self
                    .tx
                    .get(&hash)
                    .context("Unable to lookup key")?
                    .context("Unable to find key")?
                    .1;
                String::from_utf8(data.to_vec())
                    .context("Unable to convert stored data to utf-8")?
            }};
        }

        let converted: Term = match term {
            EncodedTerm::NamedNode(hash) => {
                let data = resolve!(hash.to_be_bytes());

                NamedNode::new_unchecked(data).into()
            }
            EncodedTerm::BlankNodeNumerical(node) => {
                BlankNode::new_from_unique_id(u128::from_be_bytes(node)).into()
            }
            EncodedTerm::BlankNodeSmall(value) => BlankNode::new_unchecked(value).into(),
            EncodedTerm::BlankNodeBig(hash) => {
                let iri = resolve!(hash.to_be_bytes());

                BlankNode::new_unchecked(iri).into()
            }
            EncodedTerm::LiteralStringSmall(value) => Literal::new_simple_literal(value).into(),
            EncodedTerm::LiteralStringBig(hash) => {
                let value = resolve!(hash.to_be_bytes());

                Literal::new_simple_literal(value).into()
            }
            EncodedTerm::LiteralStringLangSmallSmall(value, lang) => {
                Literal::new_language_tagged_literal_unchecked(value, lang).into()
            }
            EncodedTerm::LiteralStringLangSmallBig(value, lang) => {
                let lang = resolve!(lang.to_be_bytes());

                Literal::new_language_tagged_literal_unchecked(value, lang).into()
            }
            EncodedTerm::LiteralStringLangBigSmall(value, lang) => {
                let value = resolve!(value.to_be_bytes());

                Literal::new_language_tagged_literal_unchecked(value, lang).into()
            }
            EncodedTerm::LiteralStringLangBigBig(value, lang) => {
                let value = resolve!(value.to_be_bytes());
                let lang = resolve!(lang.to_be_bytes());

                Literal::new_language_tagged_literal_unchecked(value, lang).into()
            }
            EncodedTerm::LiteralTypedSmall(value, ty) => {
                let ty = resolve!(ty.to_be_bytes());
                let ty = NamedNode::new_unchecked(ty);

                Literal::new_typed_literal(value, ty).into()
            }
            EncodedTerm::LiteralTypedBig(value, ty) => {
                let value = resolve!(value.to_be_bytes());

                let ty = resolve!(ty.to_be_bytes());
                let ty = NamedNode::new_unchecked(ty);

                Literal::new_typed_literal(value, ty).into()
            }
            EncodedTerm::LiteralBoolean(value) => Literal::from(value).into(),
            EncodedTerm::LiteralInteger(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDecimal(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDuration(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDurationYearMonth(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDurationDayTime(value) => Literal::from(value).into(),
            EncodedTerm::LiteralFloat(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDouble(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDateTime(value) => Literal::from(value).into(),
            EncodedTerm::LiteralTime(value) => Literal::from(value).into(),
            EncodedTerm::LiteralDate(value) => Literal::from(value).into(),
            EncodedTerm::LiteralGYearMonth(value) => Literal::from(value).into(),
            EncodedTerm::LiteralGYear(value) => Literal::from(value).into(),
            EncodedTerm::LiteralGMonthDay(value) => Literal::from(value).into(),
            EncodedTerm::LiteralGDay(value) => Literal::from(value).into(),
            EncodedTerm::LiteralGMonth(value) => Literal::from(value).into(),
            EncodedTerm::Triple(triple) => {
                let EncodedTriple {
                    subject,
                    predicate,
                    object,
                } = Rc::try_unwrap(triple).unwrap_or_else(|triple| triple.as_ref().clone());

                let subject = match self.decode_term(subject)? {
                    Term::NamedNode(named) => named.into(),
                    Term::BlankNode(node) => node.into(),
                    Term::Triple(_) => anyhow::bail!("decoded a triple in subject position"),
                    Term::Literal(_) => anyhow::bail!("decoded a literal in subject position"),
                };

                let Term::NamedNode(predicate) = self.decode_term(predicate)? else {
                    anyhow::bail!("decoded a non-named-node in predicate position");
                };

                let object = self.decode_term(object)?;

                Box::new(Triple {
                    subject,
                    predicate,
                    object,
                })
                .into()
            }
        };

        Ok(converted)
    }

    fn select_from_default_graph(
        &self,
        scope: Scope<'_>,
        subject: Option<&InternalTerm<'tx, Self>>,
        predicate: Option<&InternalTerm<'tx, Self>>,
        object: Option<&InternalTerm<'tx, Self>>,
    ) -> QuadIter<'tx, Self> {
        let default_iter = select_from_default_graph(&self.tx, scope, subject, predicate, object);
        let named_iter = select_from_named_graph(&self.tx, scope, subject, predicate, object, None);

        Box::new(default_iter.chain(named_iter))
    }

    fn select_from_named_graph(
        &self,
        scope: Scope<'_>,
        subject: Option<&InternalTerm<'tx, Self>>,
        predicate: Option<&InternalTerm<'tx, Self>>,
        object: Option<&InternalTerm<'tx, Self>>,
        graph_name: &InternalTerm<'tx, Self>,
    ) -> QuadIter<'tx, Self> {
        select_from_named_graph(
            &self.tx,
            scope,
            subject,
            predicate,
            object,
            Some(graph_name),
        )
    }
}

fn select_from_named_graph<'tx, M>(
    tx: &&'tx mut Transaction<M>,
    scope: Scope<'_>,
    subject: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
    predicate: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
    object: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
    graph_name: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
) -> QuadIter<'tx, DataView<'tx, M>> {
    let (encoding, lower, upper) = tri!(partial_quad_into_range(
        scope, subject, predicate, object, graph_name
    ));

    let mut output = vec![];

    for entry in tx.range_latest(lower, upper) {
        let raw = tri!(entry);
        let quad_key: crate::domain::QuadKey<'_> = tri!(StoreKey::decode_from_bytes(&raw));

        let (subject, predicate, object, graph_name) =
            encoding.sort(quad_key.a, quad_key.b, quad_key.c, quad_key.d);

        output.push(Ok(InternalQuad {
            subject,
            predicate,
            object,
            graph_name: Some(graph_name),
        }));
    }

    Box::new(output.into_iter())
}

fn select_from_default_graph<'tx, M>(
    tx: &&'tx mut Transaction<M>,
    scope: Scope<'_>,
    subject: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
    predicate: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
    object: Option<&InternalTerm<'tx, DataView<'tx, M>>>,
) -> QuadIter<'tx, DataView<'tx, M>> {
    let (encoding, lower, upper) =
        tri!(partial_triple_into_range(scope, subject, predicate, object));

    let mut output = vec![];

    for entry in tx.range_latest(lower, upper) {
        let raw = tri!(entry);
        let triple_key: crate::domain::TripleKey<'_> = tri!(StoreKey::decode_from_bytes(&raw));

        let (subject, predicate, object) = encoding.sort(triple_key.a, triple_key.b, triple_key.c);

        output.push(Ok(InternalQuad {
            subject,
            predicate,
            object,
            graph_name: None,
        }));
    }

    Box::new(output.into_iter())
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SemanticError(#[from] anyhow::Error);

impl<'a, M> spareval::QueryableDataset<'a> for DataView<'a, M> {
    type InternalTerm = EncodedTerm;
    type Error = SemanticError;

    fn internal_quads_for_pattern(
        &self,
        subject: Option<&Self::InternalTerm>,
        predicate: Option<&Self::InternalTerm>,
        object: Option<&Self::InternalTerm>,
        graph_name: Option<Option<&Self::InternalTerm>>,
    ) -> impl Iterator<Item = Result<InternalQuad<Self::InternalTerm>, Self::Error>> + use<'a, M>
    {
        #[expect(
            clippy::expect_used,
            reason = "self.scope was round-trip validated in DataView::new, so re-decoding cannot fail"
        )]
        let scope: Scope<'_> = StoreKey::decode_from_bytes(self.scope.as_slice())
            .expect("scope was double checked before building this dataview.");

        if let Some(graph_name) = graph_name.flatten() {
            return self.select_from_named_graph(scope, subject, predicate, object, graph_name)
                as QuadIter<'a, Self>;
        }

        self.select_from_default_graph(scope, subject, predicate, object) as QuadIter<'a, Self>
    }

    fn internalize_term(&self, term: Term) -> Result<Self::InternalTerm, Self::Error> {
        Ok(encode_term_transient(&term))
    }

    fn externalize_term(&self, term: Self::InternalTerm) -> Result<Term, Self::Error> {
        Ok(self.decode_term(term)?)
    }

    fn internalize_expression_term(
        &self,
        term: ExpressionTerm,
    ) -> Result<Self::InternalTerm, Self::Error> {
        let term = match term {
            ExpressionTerm::DurationLiteral(value) => EncodedTerm::LiteralDuration(value),
            ExpressionTerm::YearMonthDurationLiteral(value) => {
                EncodedTerm::LiteralDurationYearMonth(value)
            }
            ExpressionTerm::DayTimeDurationLiteral(value) => {
                EncodedTerm::LiteralDurationDayTime(value)
            }
            ExpressionTerm::BooleanLiteral(value) => EncodedTerm::LiteralBoolean(value.into()),
            ExpressionTerm::FloatLiteral(value) => EncodedTerm::LiteralFloat(value),
            ExpressionTerm::DoubleLiteral(value) => EncodedTerm::LiteralDouble(value),
            ExpressionTerm::IntegerLiteral(value) => EncodedTerm::LiteralInteger(value),
            ExpressionTerm::DecimalLiteral(value) => EncodedTerm::LiteralDecimal(value),
            ExpressionTerm::DateTimeLiteral(value) => EncodedTerm::LiteralDateTime(value),
            ExpressionTerm::TimeLiteral(value) => EncodedTerm::LiteralTime(value),
            ExpressionTerm::DateLiteral(value) => EncodedTerm::LiteralDate(value),
            ExpressionTerm::GYearMonthLiteral(value) => EncodedTerm::LiteralGYearMonth(value),
            ExpressionTerm::GYearLiteral(value) => EncodedTerm::LiteralGYear(value),
            ExpressionTerm::GMonthDayLiteral(value) => EncodedTerm::LiteralGMonthDay(value),
            ExpressionTerm::GDayLiteral(value) => EncodedTerm::LiteralGDay(value),
            ExpressionTerm::GMonthLiteral(value) => EncodedTerm::LiteralGMonth(value),
            ExpressionTerm::Triple(triple) => {
                let ExpressionTriple {
                    subject,
                    predicate,
                    object,
                } = triple.as_ref();

                let triple = EncodedTriple {
                    subject: self.internalize_expression_term(subject.clone().into())?,
                    predicate: self.internalize_expression_term(predicate.clone().into())?,
                    object: self.internalize_expression_term(object.clone())?,
                };

                EncodedTerm::Triple(Rc::new(triple))
            }
            term => self.internalize_term(term.into())?,
        };

        Ok(term)
    }

    fn externalize_expression_term(
        &self,
        term: Self::InternalTerm,
    ) -> Result<ExpressionTerm, Self::Error> {
        let term = match term {
            EncodedTerm::LiteralDuration(value) => ExpressionTerm::DurationLiteral(value),
            EncodedTerm::LiteralDurationYearMonth(value) => {
                ExpressionTerm::YearMonthDurationLiteral(value)
            }
            EncodedTerm::LiteralDurationDayTime(value) => {
                ExpressionTerm::DayTimeDurationLiteral(value)
            }
            EncodedTerm::LiteralBoolean(value) => value.into(),
            EncodedTerm::LiteralFloat(value) => ExpressionTerm::FloatLiteral(value),
            EncodedTerm::LiteralDouble(value) => ExpressionTerm::DoubleLiteral(value),
            EncodedTerm::LiteralInteger(value) => ExpressionTerm::IntegerLiteral(value),
            EncodedTerm::LiteralDecimal(value) => ExpressionTerm::DecimalLiteral(value),
            EncodedTerm::LiteralDateTime(value) => ExpressionTerm::DateTimeLiteral(value),
            EncodedTerm::LiteralTime(value) => ExpressionTerm::TimeLiteral(value),
            EncodedTerm::LiteralDate(value) => ExpressionTerm::DateLiteral(value),
            EncodedTerm::LiteralGYearMonth(value) => ExpressionTerm::GYearMonthLiteral(value),
            EncodedTerm::LiteralGYear(value) => ExpressionTerm::GYearLiteral(value),
            EncodedTerm::LiteralGMonthDay(value) => ExpressionTerm::GMonthDayLiteral(value),
            EncodedTerm::LiteralGDay(value) => ExpressionTerm::GDayLiteral(value),
            EncodedTerm::LiteralGMonth(value) => ExpressionTerm::GMonthLiteral(value),
            EncodedTerm::Triple(triple) => {
                let EncodedTriple {
                    subject,
                    predicate,
                    object,
                } = triple.as_ref();

                let triple = ExpressionTriple::new(
                    self.externalize_expression_term(subject.clone())?,
                    self.externalize_expression_term(predicate.clone())?,
                    self.externalize_expression_term(object.clone())?,
                );

                let Some(triple) = triple else {
                    Err(anyhow::anyhow!("Unable to construct a valid triple"))?
                };

                ExpressionTerm::Triple(Box::new(triple))
            }
            term => self.decode_term(term)?.into(),
        };

        Ok(term)
    }

    fn internal_named_graphs(
        &self,
    ) -> impl Iterator<Item = Result<Self::InternalTerm, Self::Error>> + use<'a, M> {
        #[expect(
            clippy::expect_used,
            reason = "self.scope was round-trip validated in DataView::new, so re-decoding cannot fail"
        )]
        let scope: Scope<'_> = StoreKey::decode_from_bytes(self.scope.as_slice())
            .expect("scope was double checked before building this dataview.");

        let prefix = tri!(
            scope
                .only_graph_name()
                .encode()
                .context("unable to encode graph name prefix")
        );

        let mut items = vec![];

        for entry in self.tx.prefix_latest(&prefix) {
            let raw = tri!(entry);
            let key: crate::domain::GraphName<'_> = tri!(StoreKey::decode_from_bytes(&raw));
            items.push(Ok(key.name));
        }

        Box::new(items.into_iter())
    }

    fn contains_internal_graph_name(
        &self,
        graph_name: &Self::InternalTerm,
    ) -> Result<bool, Self::Error> {
        #[expect(
            clippy::expect_used,
            clippy::unwrap_in_result,
            reason = "self.scope was round-trip validated in DataView::new, so re-decoding cannot fail"
        )]
        let scope: Scope<'_> = StoreKey::decode_from_bytes(self.scope.as_slice())
            .expect("scope was double checked before building this dataview.");

        let name = scope.graph_name(graph_name.clone());

        let value = self.tx.get(&name).context("Unable to lookup graph name")?;

        Ok(value.is_some())
    }

    fn internal_term_effective_boolean_value(
        &self,
        term: EncodedTerm,
    ) -> Result<Option<bool>, Self::Error> {
        Ok(match term {
            EncodedTerm::LiteralBoolean(value) => Some(value),
            EncodedTerm::LiteralStringSmall(value) => Some(!value.is_empty()),
            EncodedTerm::LiteralStringBig { .. } => Some(false),
            EncodedTerm::LiteralFloat(value) => Some(Boolean::from(value).into()),
            EncodedTerm::LiteralDouble(value) => Some(Boolean::from(value).into()),
            EncodedTerm::LiteralInteger(value) => Some(Boolean::from(value).into()),
            EncodedTerm::LiteralDecimal(value) => Some(Boolean::from(value).into()),
            _ => None,
        })
    }
}
