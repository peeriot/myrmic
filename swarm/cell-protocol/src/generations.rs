//! Rows kept per generation of a cell: placements in [`placement_scope`] and
//! instance records in [`instance_registry_scope`].
//!
//! Each life of an SRI gets its own db key, `gen/<sri>/<gen>`, so a write or
//! delete addressed to one generation can never reach another. A node acting on
//! an outdated view, like a runtime back from the dead releasing what it still
//! believes it hosts, only ever hits the generation it saw. Readers take each
//! SRI's newest generation.
//!
//! The key carries the SRI and the generation; the value holds the rest
//! ([`PlacementRow`], [`InstanceRow`]).
//!
//! [`placement_scope`]: crate::placement_scope
//! [`instance_registry_scope`]: crate::instance_registry_scope

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::sys::collections::BTreeMap;
use crate::sys::format;
use crate::sys::string::String;
use crate::sys::vec;
use crate::sys::vec::Vec;
use crate::{BlobHash, CellInstance, Gen, PlacementEntry, PlacementKind, SpawnLineage, Sri};

/// Key prefix of every generation row in a scope.
pub const GENERATIONS: &str = "gen/";

/// The key of generation `gen_id` of `sri`.
#[must_use]
pub fn generation_key(sri: &Sri, gen_id: Gen) -> String {
    format!(
        "{}{:016x}{:032x}",
        generations_of(sri),
        gen_id.time,
        gen_id.id
    )
}

/// The key prefix covering every generation of `sri`.
#[must_use]
pub fn generations_of(sri: &Sri) -> String {
    format!("{GENERATIONS}{sri}/")
}

/// The SRI and generation a key under [`GENERATIONS`] names, `None` for a key
/// of another shape.
#[must_use]
pub fn parse_generation_key(key: &str) -> Option<(Sri, Gen)> {
    let (sri, gen_id) = key.strip_prefix(GENERATIONS)?.split_once('/')?;
    if gen_id.len() != 48 {
        return None;
    }
    let gen_id = Gen {
        time: u64::from_str_radix(gen_id.get(..16)?, 16).ok()?,
        id: u128::from_str_radix(gen_id.get(16..)?, 16).ok()?,
    };
    Some((sri.parse().ok()?, gen_id))
}

/// What a placement row stores; its key carries the SRI and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementRow {
    /// Where the cell is placed.
    pub kind: PlacementKind,
    /// The application the cell belongs to, if any.
    pub app: Option<String>,
}

/// What an instance row stores; its key carries the SRI and generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRow {
    /// The id of the class the instance was created from.
    pub class: BlobHash,
    /// The spawn edge the instance was born on.
    pub lineage: SpawnLineage,
}

impl PlacementEntry {
    /// The placement stored under the key of generation `gen_id` of `sri`.
    #[must_use]
    pub fn from_row(sri: Sri, gen_id: Gen, row: PlacementRow) -> Self {
        Self {
            sri,
            kind: row.kind,
            app: row.app,
            gen_id,
        }
    }

    /// What to store under the key of this placement's generation.
    #[must_use]
    pub fn row(&self) -> PlacementRow {
        PlacementRow {
            kind: self.kind.clone(),
            app: self.app.clone(),
        }
    }
}

impl CellInstance {
    /// The instance stored under the key of generation `gen_id` of `sri`.
    #[must_use]
    pub fn from_row(sri: Sri, gen_id: Gen, row: InstanceRow) -> Self {
        Self {
            sri,
            class: row.class,
            gen_id,
            lineage: row.lineage,
        }
    }

    /// What to store under the key of this instance's generation.
    #[must_use]
    pub fn row(&self) -> InstanceRow {
        InstanceRow {
            class: self.class,
            lineage: self.lineage.clone(),
        }
    }
}

/// Why listed generation rows could not be read.
#[derive(Debug, thiserror::Error)]
pub enum RowError {
    /// A key under [`GENERATIONS`] that is not `gen/<sri>/<gen>`.
    #[error("malformed generation key {0:?}")]
    Key(String),
    /// A value that does not decode as the scope's row type.
    #[error("undecodable row under {key:?}: {reason}")]
    Value {
        /// The row's key.
        key: String,
        /// Why it doesn't decode.
        reason: postcard::Error,
    },
}

/// The newest placement of each SRI among `entries`, as listed (keys with
/// values) under [`GENERATIONS`] or [`generations_of`] in the placement scope.
/// Rows that can't be read come back as errors after the readable ones, for
/// the caller to fail on or skip.
pub fn newest_placements(entries: Vec<(String, Vec<u8>)>) -> Vec<Result<PlacementEntry, RowError>> {
    newest(entries, PlacementEntry::from_row)
}

/// The newest instance of each SRI among `entries`, as listed (keys with
/// values) under [`GENERATIONS`] or [`generations_of`] in the instance
/// registry scope. Rows that can't be read come back as errors after the
/// readable ones, for the caller to fail on or skip.
pub fn newest_instances(entries: Vec<(String, Vec<u8>)>) -> Vec<Result<CellInstance, RowError>> {
    newest(entries, CellInstance::from_row)
}

/// Keeps each SRI's newest generation among `entries` and assembles it from
/// its key and decoded value; the errors go last.
fn newest<R, T>(
    entries: Vec<(String, Vec<u8>)>,
    assemble: fn(Sri, Gen, R) -> T,
) -> Vec<Result<T, RowError>>
where
    R: DeserializeOwned,
{
    let mut malformed = vec![];
    let mut newest = BTreeMap::<Sri, (Gen, String, Vec<u8>)>::new();
    for (key, value) in entries {
        match parse_generation_key(&key) {
            Some((sri, gen_id)) => {
                if newest
                    .get(&sri)
                    .is_none_or(|(newest_gen, _, _)| *newest_gen < gen_id)
                {
                    newest.insert(sri, (gen_id, key, value));
                }
            }
            None => malformed.push(Err(RowError::Key(key))),
        }
    }

    newest
        .into_iter()
        .map(|(sri, (gen_id, key, value))| {
            let row =
                postcard::from_bytes(&value).map_err(|reason| RowError::Value { key, reason })?;
            Ok(assemble(sri, gen_id, row))
        })
        .chain(malformed)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sri_of_path;

    fn sri(path: &str) -> Sri {
        sri_of_path(path).unwrap().into()
    }

    fn entry(sri: Sri, gen_id: Gen, app: &str) -> (String, Vec<u8>) {
        let row = PlacementRow {
            kind: PlacementKind::Placeholder,
            app: Some(app.into()),
        };
        (
            generation_key(&sri, gen_id),
            postcard::to_allocvec(&row).unwrap(),
        )
    }

    #[test]
    fn a_generation_key_parses_back() {
        let gen_id = Gen::from_parts(u64::MAX, 1 << 100);
        let key = generation_key(&sri("a"), gen_id);
        assert!(key.starts_with(&generations_of(&sri("a"))));
        assert_eq!(parse_generation_key(&key), Some((sri("a"), gen_id)));
    }

    #[test]
    fn the_newest_generation_of_each_sri_wins() {
        let (a, b) = (sri("a"), sri("b"));
        let placements = newest_placements(vec![
            entry(a, Gen::from_parts(2, 1), "a-new"),
            entry(a, Gen::from_parts(1, 9), "a-old"),
            entry(b, Gen::from_parts(1, 1), "b"),
        ]);
        assert_eq!(placements.len(), 2);
        for placement in placements {
            let placement = placement.unwrap();
            let expected = if placement.sri == a { "a-new" } else { "b" };
            assert_eq!(placement.app.as_deref(), Some(expected));
        }
    }

    #[test]
    fn a_malformed_key_is_an_error_after_the_readable_rows() {
        let good = entry(sri("a"), Gen::from_parts(1, 1), "a");
        let malformed = (String::from("gen/not-a-key"), good.1.clone());
        let placements = newest_placements(vec![malformed, good]);
        assert_eq!(placements.len(), 2);
        assert!(placements[0].is_ok());
        assert!(matches!(placements[1], Err(RowError::Key(_))));
    }

    #[test]
    fn a_placement_round_trips_through_its_row() {
        let placement = PlacementEntry {
            sri: sri("a"),
            kind: PlacementKind::Bridge { sri: sri("bridge") },
            app: Some("app".into()),
            gen_id: Gen::from_parts(3, 1),
        };
        let bytes = postcard::to_allocvec(&placement.row()).unwrap();
        let row = postcard::from_bytes::<PlacementRow>(&bytes).unwrap();
        assert_eq!(
            PlacementEntry::from_row(placement.sri, placement.gen_id, row),
            placement
        );
    }

    #[test]
    fn an_instance_round_trips_through_its_row() {
        let instance = CellInstance {
            sri: sri("a"),
            class: BlobHash::of(b"c"),
            gen_id: Gen::from_parts(7, 1),
            lineage: SpawnLineage {
                parent: None,
                parent_gen_id: Some(Gen::from_parts(9, 1)),
                detached: true,
                local_name: Some("pump".into()),
                grace_ms: Some(30_000),
                deadline_ms: Some(120_000),
            },
        };
        let bytes = postcard::to_allocvec(&instance.row()).unwrap();
        let row = postcard::from_bytes::<InstanceRow>(&bytes).unwrap();
        assert_eq!(
            CellInstance::from_row(instance.sri, instance.gen_id, row),
            instance
        );
    }
}
