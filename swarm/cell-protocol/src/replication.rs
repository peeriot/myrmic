//! Manually configured replication sets: what gets replicated, and by which
//! nodes.
//!
//! An entry pairs a [`ReplicaSelector`] — what to replicate — with a set of
//! node tags. A db node holds a replica when it carries any one of an entry's
//! tags. Entries live in the network-replicated `sys` namespace (see
//! [`replication_scope`]), so every node — including one that joins later —
//! converges on the same configuration and decides its own participation
//! locally.

use core::fmt::{self, Display};
use core::str::FromStr;

use db_commons::models::{NodeId, Scope, Subject};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sys::string::{String, ToString};
use crate::sys::vec::Vec;
use crate::{NAMESPACE_CELLS, NameError, RuntimeId, Sri, scope_of_cell};

/// Database of the replication-set configuration
const REPLICATION_DB: &str = "replication";
/// Table of the configured replication sets
pub const REPLICATION_TABLE: &str = "entries";
/// Table of the provisional custody rows, in the same scope as
/// [`REPLICATION_TABLE`]
pub const CUSTODY_TABLE: &str = "custody";

/// Prefix marking a selector as naming an application.
const APP_PREFIX: &str = "app:";
/// Prefix marking a selector as a raw slice of the scope hierarchy.
const SCOPE_PREFIX: &str = "scope:";

/// Returns the DB scope holding the configured replication sets.
///
/// Lives in the `sys` namespace, which every db node replicates
/// unconditionally, so configuration reaches nodes that join later.
pub fn replication_scope() -> Scope {
    Scope::new(db_commons::NAMESPACE_SYS, REPLICATION_DB, "p")
}

/// Prefix of the tags the system defines. A user-written tag may never start
/// with it, so a system tag is always exactly what the system stamped.
pub const SYSTEM_TAG_PREFIX: char = '@';

/// The system tag each runtime carries for itself: `@<runtime id>`. An
/// entry naming it pins a replica to that one runtime.
#[must_use]
pub fn runtime_tag(id: RuntimeId) -> String {
    crate::sys::format!("{SYSTEM_TAG_PREFIX}{id}")
}

/// A user-written tag used the system prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedTag;

impl Display for ReservedTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tags starting with '{SYSTEM_TAG_PREFIX}' are system-defined",
        )
    }
}

#[cfg(not(target_os = "none"))]
impl std::error::Error for ReservedTag {}

/// Checks a tag a node is configured to carry. The system prefix is refused
/// outright, even for a well-formed runtime tag: a tag a node *carries* with
/// the prefix is always one the system stamped itself. Replication-set entries
/// are not checked — naming a system tag there is how a set pins to one.
pub fn check_user_tag(tag: &str) -> Result<(), ReservedTag> {
    if tag.starts_with(SYSTEM_TAG_PREFIX) {
        return Err(ReservedTag);
    }

    Ok(())
}

/// What a replication entry targets.
///
/// Parsed from the identifier a user writes; [`Display`] renders the canonical
/// form, which is also the entry's key in the replication table. The two round
/// trip.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReplicaSelector {
    /// Every scope of every cell registered to this application.
    App(String),
    /// Every scope of one cell.
    Cell(Sri),
    /// A slice of the scope hierarchy, named directly.
    Subject(Subject),
}

impl ReplicaSelector {
    /// The scopes this selector covers, given the cell registry.
    ///
    /// `cells` supplies the `(sri, app)` pair of every registered cell; only
    /// [`ReplicaSelector::App`] consults it. Resolution is deliberately
    /// deferred to the node applying the configuration rather than done when
    /// the entry is written, so a cell deployed into an application later is
    /// picked up without reconfiguration.
    pub fn subjects<'a, I>(&self, cells: I) -> Vec<Subject>
    where
        I: IntoIterator<Item = (&'a Sri, Option<&'a str>)>,
    {
        match self {
            Self::App(name) => cells
                .into_iter()
                .filter(|(_, app)| *app == Some(name.as_str()))
                .map(|(sri, _)| subject_of_cell(*sri))
                .collect(),
            Self::Cell(sri) => Vec::from([subject_of_cell(*sri)]),
            Self::Subject(subject) => Vec::from([subject.clone()]),
        }
    }
}

/// Every scope belonging to one cell.
///
/// [`scope_of_cell`] names a cell's public schema; replication of a cell means
/// all of its schemas, so the schema level is left unconstrained.
fn subject_of_cell(sri: Sri) -> Subject {
    let scope = scope_of_cell(sri);
    Subject::Database(String::from(NAMESPACE_CELLS), scope.database)
}

/// Why a replication-set identifier was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorError {
    /// The identifier was empty.
    Empty,
    /// An `app:` selector named no application.
    EmptyApp,
    /// A `scope:` selector had no segments, an empty segment, or more than the
    /// three the hierarchy has.
    BadScope,
    /// The identifier parsed as neither a UUID nor a well-formed SRN path.
    BadName(NameError),
}

impl Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty replication identifier"),
            Self::EmptyApp => f.write_str("`app:` needs an application name"),
            Self::BadScope => f.write_str(
                "`scope:` takes `namespace`, `namespace/database`, or `namespace/database/schema`",
            ),
            Self::BadName(err) => write!(f, "not a UUID, and not a valid SRN: {err}"),
        }
    }
}

#[cfg(not(target_os = "none"))]
impl std::error::Error for SelectorError {}

impl FromStr for ReplicaSelector {
    type Err = SelectorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(SelectorError::Empty);
        }

        if let Some(app) = s.strip_prefix(APP_PREFIX) {
            if app.is_empty() {
                return Err(SelectorError::EmptyApp);
            }
            return Ok(Self::App(String::from(app)));
        }

        if let Some(scope) = s.strip_prefix(SCOPE_PREFIX) {
            return parse_subject(scope).map(Self::Subject);
        }

        // The edge rule: a UUID is an SRI verbatim, anything else is an SRN
        // path. `Sri::from_target` folds the path, which also validates it —
        // so `chatty/` is rejected as an empty segment.
        Sri::from_target(s)
            .map(Self::Cell)
            .map_err(SelectorError::BadName)
    }
}

fn parse_subject(s: &str) -> Result<Subject, SelectorError> {
    let mut segments = s.split('/');

    // `split` always yields at least one item, so the levels below only fail
    // the arity check when a segment is empty or a fourth one shows up.
    let namespace = segments.next().unwrap_or_default();
    let database = segments.next();
    let schema = segments.next();

    if segments.next().is_some() || namespace.is_empty() {
        return Err(SelectorError::BadScope);
    }

    match (database, schema) {
        (None, _) => Ok(Subject::Namespace(String::from(namespace))),
        (Some(database), None) if !database.is_empty() => Ok(Subject::Database(
            String::from(namespace),
            String::from(database),
        )),
        (Some(database), Some(schema)) if !database.is_empty() && !schema.is_empty() => {
            Ok(Subject::Scope(Scope::new(namespace, database, schema)))
        }
        _ => Err(SelectorError::BadScope),
    }
}

impl Display for ReplicaSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::App(name) => write!(f, "{APP_PREFIX}{name}"),
            Self::Cell(sri) => Display::fmt(sri, f),
            Self::Subject(Subject::Namespace(ns)) => write!(f, "{SCOPE_PREFIX}{ns}"),
            Self::Subject(Subject::Database(ns, db)) => write!(f, "{SCOPE_PREFIX}{ns}/{db}"),
            Self::Subject(Subject::Scope(scope)) => write!(f, "{SCOPE_PREFIX}{scope}"),
        }
    }
}

/// One configured replication set, as stored in [`replication_scope`]'s
/// [`REPLICATION_TABLE`]. Keyed by the selector's canonical string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaEntry {
    /// What to replicate.
    pub selector: ReplicaSelector,
    /// Tags of the nodes that should hold a replica. A node participates when
    /// it carries any one of them; an entry with no tags is not stored.
    pub tags: Vec<String>,
    /// The identifier as it was written, when that carries more meaning than
    /// the canonical form — an SRN, whose SRI is a one-way hash. Display only.
    pub label: Option<String>,
}

impl ReplicaEntry {
    /// A new entry for `selector`, recording `label` only when it differs from
    /// the canonical form.
    pub fn new(selector: ReplicaSelector, tags: Vec<String>, label: &str) -> Self {
        let label = (label != selector.to_string()).then(|| String::from(label));

        Self {
            selector,
            tags,
            label,
        }
    }

    /// The entry's key in the replication table.
    pub fn key(&self) -> String {
        self.selector.to_string()
    }

    /// How to name this entry to a user: the identifier they wrote, else the
    /// canonical form.
    pub fn display_name(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.selector.to_string())
    }

    /// Whether a node carrying `node_tags` should hold this replica.
    pub fn matches(&self, node_tags: &[String]) -> bool {
        self.tags.iter().any(|tag| node_tags.contains(tag))
    }
}

/// One provisional custody claim, as stored in [`replication_scope`]'s
/// [`CUSTODY_TABLE`]: `node` auto-promoted itself into a replica for `scope`
/// because no configured replica was locatable.
///
/// Custody lives outside [`ReplicaEntry`] so the runtime never mutates human
/// intent, and each node writes and deletes only its own row — conflict-free
/// by construction. Two rows for one scope becoming visible after a netsplit
/// heals is the collapse trigger: every node computes the same
/// [`custody_winner`], and the losers drain toward it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyRow {
    /// The scope held under provisional custody.
    pub scope: Scope,
    /// The custodian; only this node may write or delete the row.
    pub node: NodeId,
}

impl CustodyRow {
    /// A custody claim on `scope` by `node`.
    #[must_use]
    pub fn new(scope: Scope, node: NodeId) -> Self {
        Self { scope, node }
    }

    /// The row's key in the custody table, unique per `(scope, node)`.
    pub fn key(&self) -> String {
        let mut key = self.scope.to_string();
        key.push('@');
        for byte in &self.node {
            let _ = core::fmt::write(&mut key, format_args!("{byte:02x}"));
        }
        key
    }
}

/// The rendezvous winner among the live provisional custodians of `scope`:
/// argmax of `hash(scope, node)`, raw id as the final tie-break.
///
/// A pure function of stable, already-shared state, so any node computes the
/// same winner at any time with zero message exchange. Scope-dependent, so
/// custody spreads across nodes instead of one id accumulating every scope.
/// The hash itself lives in `db_commons` so `db-client`'s scoped `any_node`
/// fallback draws with the same function.
#[must_use]
pub fn custody_winner(scope: &Scope, nodes: impl IntoIterator<Item = NodeId>) -> Option<NodeId> {
    use db_commons::models::rendezvous_hash;

    nodes
        .into_iter()
        .max_by_key(|node| (rendezvous_hash(scope, node), *node))
}

/// Whether `s` looks like a UUID, and so names a cell directly.
///
/// Exposed for callers rendering an identifier: a UUID has no useful label.
#[must_use]
pub fn is_uuid(s: &str) -> bool {
    Uuid::try_parse(s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ReplicaSelector {
        s.parse().expect("selector should parse")
    }

    #[test]
    fn app_prefix_selects_an_application() {
        assert_eq!(
            parse("app:chatty"),
            ReplicaSelector::App(String::from("chatty")),
        );
    }

    #[test]
    fn uuid_selects_that_cell() {
        let sri = Sri::of_path("chatty").unwrap();
        assert_eq!(parse(&sri.to_string()), ReplicaSelector::Cell(sri));
    }

    #[test]
    fn bare_name_is_an_srn() {
        assert_eq!(
            parse("chatty"),
            ReplicaSelector::Cell(Sri::of_path("chatty").unwrap()),
        );
    }

    #[test]
    fn srn_path_is_exact_not_a_suffix_match() {
        let server = parse("chatty/server");
        assert_eq!(
            server,
            ReplicaSelector::Cell(Sri::of_path("chatty/server").unwrap()),
        );
        assert_ne!(server, parse("other/chatty/server"));
    }

    #[test]
    fn trailing_separator_is_an_error() {
        assert_eq!(
            "chatty/".parse::<ReplicaSelector>(),
            Err(SelectorError::BadName(NameError::EmptySegment)),
        );
    }

    #[test]
    fn empty_identifier_is_an_error() {
        assert_eq!("".parse::<ReplicaSelector>(), Err(SelectorError::Empty));
        assert_eq!(
            "app:".parse::<ReplicaSelector>(),
            Err(SelectorError::EmptyApp),
        );
    }

    #[test]
    fn scope_prefix_walks_the_hierarchy() {
        assert_eq!(
            parse("scope:sys"),
            ReplicaSelector::Subject(Subject::Namespace(String::from("sys"))),
        );
        assert_eq!(
            parse("scope:sys/rep"),
            ReplicaSelector::Subject(Subject::Database(String::from("sys"), String::from("rep"),)),
        );
        assert_eq!(
            parse("scope:sys/rep/info"),
            ReplicaSelector::Subject(Subject::Scope(Scope::new("sys", "rep", "info"))),
        );
    }

    #[test]
    fn malformed_scopes_are_rejected() {
        for input in ["scope:", "scope:sys/", "scope:sys//info", "scope:a/b/c/d"] {
            assert_eq!(
                input.parse::<ReplicaSelector>(),
                Err(SelectorError::BadScope),
                "{input} should be rejected",
            );
        }
    }

    #[test]
    fn canonical_form_round_trips() {
        for input in [
            "app:chatty",
            "scope:sys",
            "scope:sys/rep",
            "scope:sys/rep/info",
            "chatty/server",
        ] {
            let selector = parse(input);
            assert_eq!(parse(&selector.to_string()), selector, "{input}");
        }
    }

    #[test]
    fn cell_canonicalises_to_its_uuid() {
        let selector = parse("chatty/server");
        let sri = Sri::of_path("chatty/server").unwrap();
        assert_eq!(selector.to_string(), sri.to_string());
    }

    #[test]
    fn label_kept_only_when_it_adds_something() {
        let selector = parse("chatty/server");
        let entry = ReplicaEntry::new(selector.clone(), vec![], "chatty/server");
        assert_eq!(entry.display_name(), "chatty/server");

        let canonical = selector.to_string();
        let entry = ReplicaEntry::new(selector, vec![], &canonical);
        assert_eq!(entry.label, None);
        assert_eq!(entry.display_name(), canonical);
    }

    #[test]
    fn a_node_participates_on_any_shared_tag() {
        let entry = ReplicaEntry::new(
            parse("app:chatty"),
            vec![String::from("region-1"), String::from("region-2")],
            "app:chatty",
        );

        assert!(entry.matches(&[String::from("region-2")]));
        assert!(!entry.matches(&[String::from("region-3")]));
        assert!(!entry.matches(&[]));
    }

    #[test]
    fn app_expands_to_every_cell_registered_to_it() {
        let server = Sri::of_path("chatty/server").unwrap();
        let client = Sri::of_path("chatty/client").unwrap();
        let other = Sri::of_path("other").unwrap();
        let cells = [
            (&server, Some("chatty")),
            (&client, Some("chatty")),
            (&other, Some("elsewhere")),
            (&other, None),
        ];

        let subjects = parse("app:chatty").subjects(cells);

        assert_eq!(
            subjects,
            vec![subject_of_cell(server), subject_of_cell(client)],
        );
    }

    #[test]
    fn a_cell_covers_all_of_its_schemas() {
        let sri = Sri::of_path("chatty").unwrap();
        let subjects = parse("chatty").subjects([]);

        assert_eq!(
            subjects,
            vec![Subject::Database(
                String::from(NAMESPACE_CELLS),
                sri.to_string(),
            )],
        );
    }

    #[test]
    fn a_subject_selector_passes_straight_through() {
        assert_eq!(
            parse("scope:sys/rep").subjects([]),
            vec![Subject::Database(String::from("sys"), String::from("rep"))],
        );
    }

    #[test]
    fn the_runtime_tag_carries_the_runtime_id() {
        let id: RuntimeId = "a0b1c2".parse().expect("runtime id");

        assert_eq!(runtime_tag(id), "@a0b1c2");
    }

    #[test]
    fn plain_user_tags_pass() {
        assert_eq!(check_user_tag("region-1"), Ok(()));
    }

    #[test]
    fn user_tags_with_the_system_prefix_are_rejected() {
        let id: RuntimeId = "a0b1c2".parse().expect("runtime id");

        // Even a well-formed runtime tag: only the system stamps those.
        assert_eq!(check_user_tag(&runtime_tag(id)), Err(ReservedTag));
        assert_eq!(check_user_tag("@mine"), Err(ReservedTag));
    }

    fn node(n: u8) -> NodeId {
        [n; 16]
    }

    #[test]
    fn custody_keys_are_unique_per_scope_and_node() {
        let scope = Scope::new("tele", "telemetry", "p");
        let other = Scope::new("tele", "telemetry", "q");

        let key = CustodyRow::new(scope.clone(), node(1)).key();

        assert_eq!(key, CustodyRow::new(scope.clone(), node(1)).key());
        assert_ne!(key, CustodyRow::new(scope.clone(), node(2)).key());
        assert_ne!(key, CustodyRow::new(other, node(1)).key());
    }

    #[test]
    fn custody_winner_is_deterministic_and_order_independent() {
        let scope = Scope::new("tele", "telemetry", "p");
        let nodes = [node(1), node(2), node(3)];
        let mut reversed = nodes;
        reversed.reverse();

        let winner = custody_winner(&scope, nodes).expect("winner");

        assert_eq!(custody_winner(&scope, reversed), Some(winner));
        assert!(nodes.contains(&winner));
    }

    #[test]
    fn custody_winner_depends_on_the_scope() {
        // Rendezvous hashing spreads custody across nodes instead of letting
        // one id accumulate every scope. With a fixed hash this is a stable
        // assertion, not a probabilistic one.
        let nodes = [node(1), node(2)];

        let winners: Vec<_> = (0..8)
            .map(|i| {
                let scope = Scope::new("ns", "db", crate::sys::format!("s{i}"));
                custody_winner(&scope, nodes).expect("winner")
            })
            .collect();

        assert!(
            winners.iter().any(|w| *w != winners[0]),
            "every scope hashed to the same node",
        );
    }

    #[test]
    fn custody_winner_of_one_is_that_node() {
        let scope = Scope::new("ns", "db", "s");

        assert_eq!(custody_winner(&scope, [node(7)]), Some(node(7)));
        assert_eq!(custody_winner(&scope, []), None);
    }
}
