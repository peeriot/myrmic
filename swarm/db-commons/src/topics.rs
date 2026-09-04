use alloc::string::ToString;

// Used in conjuction with standard operating messages.
// Things such as "start transaction", "insert key", "commit", etc are all ran over this topic.
pub const DATA_V1_QUERY: &str = "@db/@v1/@-query/";

// Used in conjuction with standard operating messages.
// Things such as "start transaction", "insert key", "commit", etc are all ran over this topic.
pub fn format_query<S: core::fmt::Display>(target: S) -> alloc::string::String {
    let mut query = target.to_string();
    query.insert_str(0, DATA_V1_QUERY);
    query
}

pub mod replica {
    use alloc::format;
    use core::str::FromStr;

    pub const REPLICA_PREFIX: &str = "@db/@v1/@-replica";

    pub fn parse_sender(ke: &str) -> Result<uhlc::ID, alloc::string::String> {
        let (rest, _target) = ke.rsplit_once('/').ok_or_else(|| {
            alloc::string::String::from("unable to parse sender id: unable to find '/'")
        })?;
        let (_rest, sender) = rest.rsplit_once('/').ok_or_else(|| {
            alloc::string::String::from("unable to parse sender id: unable to find '/'")
        })?;

        let id = uhlc::ID::from_str(sender)
            .map_err(|err| format!("unable to parse sender id: {}", err.cause))?;

        Ok(id)
    }

    // Used via the replica client, this is used to send messages between nodes that are replicating a given subject.
    // The `key` is the unique identifier for the subject being replicated.
    pub fn format_replica(
        namespace: impl core::fmt::Display,
        database: impl core::fmt::Display,
        schema: impl core::fmt::Display,
        sender: impl core::fmt::Display,
        target: impl core::fmt::Display,
    ) -> alloc::string::String {
        format!("{REPLICA_PREFIX}/{namespace}/{database}/{schema}/{sender}/{target}")
    }
}

pub mod replica_sync {
    use alloc::format;
    use alloc::string::String;

    /// Per-holder, per-subject queryable answering direct catch-up pulls and
    /// coverage checks (`models::replication::sync`). The node id routes the
    /// query to one specific holder; a holder of a broad subject declares it
    /// with wildcard scope chunks, which zenoh intersects with the concrete
    /// scope a query names.
    pub const SYNC_PREFIX: &str = "@db/@v1/@-sync";

    pub fn format(
        node: impl core::fmt::Display,
        namespace: impl core::fmt::Display,
        database: impl core::fmt::Display,
        schema: impl core::fmt::Display,
    ) -> String {
        format!("{SYNC_PREFIX}/{node}/{namespace}/{database}/{schema}")
    }
}

pub mod replica_query {
    use alloc::format;
    use alloc::string::String;

    // Per-subject queryable used by clients to discover which replicating nodes
    // hold a given scope (and at what version). Keyed by the concrete scope so
    // zenoh only routes the query to nodes replicating a covering subject.
    pub const LOCATE_PREFIX: &str = "@db/@v1/@-locate";

    pub fn format(
        namespace: impl core::fmt::Display,
        database: impl core::fmt::Display,
        schema: impl core::fmt::Display,
    ) -> String {
        format!("{LOCATE_PREFIX}/{namespace}/{database}/{schema}")
    }

    /// Splits a locate keyexpr back into `(namespace, database, schema)`.
    pub fn parse_scope(ke: &str) -> Result<(&str, &str, &str), String> {
        let rest = ke
            .strip_prefix(LOCATE_PREFIX)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| String::from("not a locate keyexpr"))?;

        let mut chunks = rest.split('/');
        let parsed = (chunks.next(), chunks.next(), chunks.next());

        let ((Some(ns), Some(db), Some(schema)), None) = (parsed, chunks.next()) else {
            return Err(String::from(
                "expected exactly three chunks after the locate prefix",
            ));
        };

        Ok((ns, db, schema))
    }
}

pub mod events {
    use alloc::format;
    use alloc::string::String;

    // Commit notifications for table writes are published here, one message
    // per touched table. Subscribers pick their granularity with wildcards.
    pub const EVENTS_PREFIX: &str = "@db/@v1/@-events";

    pub fn format_event(
        namespace: impl core::fmt::Display,
        database: impl core::fmt::Display,
        schema: impl core::fmt::Display,
        table: impl core::fmt::Display,
    ) -> String {
        format!("{EVENTS_PREFIX}/{namespace}/{database}/{schema}/{table}")
    }

    /// Splits an event keyexpr back into `(namespace, database, schema, table)`.
    pub fn parse_event(ke: &str) -> Result<(&str, &str, &str, &str), String> {
        let rest = ke
            .strip_prefix(EVENTS_PREFIX)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| String::from("not an event keyexpr"))?;

        let mut chunks = rest.split('/');
        let parsed = (chunks.next(), chunks.next(), chunks.next(), chunks.next());

        let ((Some(ns), Some(db), Some(schema), Some(table)), None) = (parsed, chunks.next())
        else {
            return Err(String::from(
                "expected exactly four chunks after the event prefix",
            ));
        };

        Ok((ns, db, schema, table))
    }
}

#[cfg(test)]
mod tests {
    use super::{events, replica_query};

    #[test]
    fn locate_keyexpr_roundtrips() {
        let ke = replica_query::format("ns", "db", "schema");
        assert_eq!(ke, "@db/@v1/@-locate/ns/db/schema");

        let parsed = replica_query::parse_scope(&ke).unwrap();
        assert_eq!(parsed, ("ns", "db", "schema"));
    }

    #[test]
    fn parse_scope_rejects_foreign_keyexprs() {
        assert!(replica_query::parse_scope("@db/@v1/@-events/ns/db/schema").is_err());
        assert!(replica_query::parse_scope("@db/@v1/@-locate/only/two").is_err());
        assert!(replica_query::parse_scope("@db/@v1/@-locate/one/too/many/chunks").is_err());
    }

    #[test]
    fn event_keyexpr_roundtrips() {
        let ke = events::format_event("ns", "db", "schema", "letters");
        assert_eq!(ke, "@db/@v1/@-events/ns/db/schema/letters");

        let parsed = events::parse_event(&ke).unwrap();
        assert_eq!(parsed, ("ns", "db", "schema", "letters"));
    }

    #[test]
    fn parse_event_rejects_foreign_keyexprs() {
        assert!(events::parse_event("@db/@v1/@-query/xyz").is_err());
        assert!(events::parse_event("@db/@v1/@-events/only/three/chunks").is_err());
        assert!(events::parse_event("@db/@v1/@-events/one/too/many/chunks/here").is_err());
    }
}
