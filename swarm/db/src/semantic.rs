use anyhow::Context;
use std::sync::Arc;
pub use term::{EncodedQuad, EncodedTerm, EncodedTriple};
pub use unify::{convert_graph_name, convert_triple, fill_ground_quad_pattern, fill_quad_pattern};

pub(crate) use term::{encode_term_collected, encode_term_transient};

pub use spareval;

pub use oxrdf::Term as OxTerm;
use spargebra::SparqlParser;

mod depth;
mod term;
pub mod unify;

/// Parsing, planning and evaluating recurse up to [`depth::MAX_DEPTH`] times,
/// using at most ~60 KB a step in debug builds and far less in release ones.
const STACK_SIZE: usize = 32 << 20;
const STACK_RED_ZONE: usize = 16 << 20;

/// Runs `f` with at least [`STACK_RED_ZONE`] of stack. No thread has that
/// much to spare, so in practice `f` runs on a fresh [`STACK_SIZE`] segment
/// (a few microseconds to map) unless already inside one.
pub(crate) fn with_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(STACK_RED_ZONE, STACK_SIZE, f)
}

#[derive(Debug)]
pub struct Update {
    pub(crate) raw: spargebra::Update,
}

impl Update {
    pub fn parse(update: &str, base_iri: Option<&str>) -> anyhow::Result<Self> {
        depth::check(update).context("unable to parse update")?;

        let mut parser = SparqlParser::new();

        if let Some(base_iri) = base_iri {
            parser = parser.with_base_iri(base_iri).context("invalid base iri")?;
        }
        let update =
            with_stack(|| parser.parse_update(update)).context("unable to parse update")?;

        Ok(Self { raw: update })
    }
}

#[derive(Debug)]
pub enum QueryKind {
    Select,
    Construct(Vec<spargebra::term::TriplePattern>),
    Describe,
    Ask,
}

impl QueryKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Ask => "ASK",
            Self::Construct(_) => "CONSTRUCT",
            Self::Describe => "DESCRIBE",
        }
    }
}

#[derive(Debug)]
pub struct Query {
    pub(crate) dataset: Option<spargebra::algebra::QueryDataset>,
    pub(crate) base_iri: Option<oxiri::Iri<String>>,
    pub(crate) pattern: spargebra::algebra::GraphPattern,
    pub(crate) kind: QueryKind,
}

impl Query {
    pub fn parse(query: &str, base_iri: Option<&str>) -> anyhow::Result<Self> {
        depth::check(query).context("unable to parse query")?;

        let mut parser = SparqlParser::new();

        if let Some(base_iri) = base_iri {
            parser = parser.with_base_iri(base_iri).context("invalid base iri")?;
        }
        let query = with_stack(|| parser.parse_query(query)).context("unable to parse query")?;

        Ok(query.into())
    }

    pub fn kind(&self) -> &QueryKind {
        &self.kind
    }
}

impl From<spargebra::Query> for Query {
    fn from(query: spargebra::Query) -> Self {
        match query {
            spargebra::Query::Select {
                dataset,
                pattern,
                base_iri,
            } => Self {
                dataset,
                base_iri,
                pattern,
                kind: QueryKind::Select,
            },
            spargebra::Query::Construct {
                template,
                dataset,
                pattern,
                base_iri,
            } => Self {
                dataset,
                base_iri,
                pattern,
                kind: QueryKind::Construct(template),
            },
            spargebra::Query::Describe {
                dataset,
                pattern,
                base_iri,
            } => Self {
                dataset,
                base_iri,
                pattern,
                kind: QueryKind::Describe,
            },
            spargebra::Query::Ask {
                dataset,
                pattern,
                base_iri,
            } => Self {
                dataset,
                base_iri,
                pattern,
                kind: QueryKind::Ask,
            },
        }
    }
}

impl From<Query> for spargebra::Query {
    fn from(query: Query) -> Self {
        let Query {
            dataset,
            base_iri,
            pattern,
            kind,
        } = query;

        match kind {
            QueryKind::Select => spargebra::Query::Select {
                dataset,
                pattern,
                base_iri,
            },
            QueryKind::Construct(patterns) => spargebra::Query::Construct {
                template: patterns,
                dataset,
                pattern,
                base_iri,
            },
            QueryKind::Describe => spargebra::Query::Describe {
                dataset,
                pattern,
                base_iri,
            },
            QueryKind::Ask => spargebra::Query::Ask {
                dataset,
                pattern,
                base_iri,
            },
        }
    }
}

#[derive(Debug)]
pub struct QuerySolution {
    pub variables: Vec<String>,
    pub solutions: Vec<Vec<Option<OxTerm>>>,
}

impl From<QuerySolution> for Vec<spareval::QuerySolution> {
    fn from(value: QuerySolution) -> Self {
        #[expect(
            clippy::unwrap_used,
            reason = "names came from already-valid oxrdf Variables, so re-parsing cannot fail"
        )]
        let vars = value
            .variables
            .into_iter()
            .map(|var| oxrdf::Variable::new(var).unwrap())
            .collect::<Vec<_>>();

        let vars: Arc<[oxrdf::Variable]> = Arc::from(vars);

        let mut output = vec![];
        for solution in value.solutions {
            output.push(spareval::QuerySolution::from((vars.clone(), solution)));
        }

        output
    }
}
