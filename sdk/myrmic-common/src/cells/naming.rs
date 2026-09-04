//! Deterministic SRI ↔ SRN mapping.
//!
//! An **SRN** (self-referential *name*) is a human-readable path such as
//! `myapp/gateway/worker-1`. An **SRI** is a cell's UUID identity. The two are
//! linked by a pure function: the SRI is the fold of UUIDv5 over the path's
//! segments, starting from [`ROOT_NS`]. A child's namespace is its parent's
//! SRI, so the spawn tree and the name tree are the same structure.
//!
//! - **SRN → SRI** is total, offline, and deterministic ([`sri_of_path`],
//!   [`child_sri`]). For a well-known target it collapses to a compile-time
//!   constant.
//! - **SRI → SRN** is *not* recoverable (it's a hash); the registry stores the
//!   SRN string as display metadata.
//!
//! By convention the first path segment names the app; there is no special
//! casing for it — it is simply the first fold from [`ROOT_NS`].

use core::fmt;

use uuid::Uuid;

/// Root namespace every SRN path folds from.
///
/// Reads as `ALL CELLS` (`a11ce115`) followed by `dead beef cafe coffee decaf`.
/// This value is load-bearing: changing it shifts **every** cell identity in
/// the network, so it must never change.
pub const ROOT_NS: Uuid = Uuid::from_u128(0xa11c_e115_dead_beef_cafe_c0ff_ee1d_ecaf);

/// Separator between SRN path segments.
pub const SEP: char = '/';

/// Why an SRN (a whole path or a single segment) was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// A segment was empty — e.g. an empty string, or a leading, trailing, or
    /// doubled separator.
    EmptySegment,
    /// A segment did not start with `[a-z0-9]`.
    BadStart,
    /// A segment contained a character outside `[a-z0-9._-]`.
    InvalidChar,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            NameError::EmptySegment => "empty SRN segment (check for leading/trailing/doubled '/')",
            NameError::BadStart => "SRN segment must start with a lowercase letter or digit",
            NameError::InvalidChar => "SRN segment may only contain [a-z0-9._-]",
        };
        f.write_str(msg)
    }
}

/// Validate a single SRN path segment (a local name) against the grammar
/// `^[a-z0-9][a-z0-9._-]*$`.
pub fn validate_segment(seg: &str) -> Result<(), NameError> {
    let mut chars = seg.chars();
    match chars.next() {
        None => return Err(NameError::EmptySegment),
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        Some(_) => return Err(NameError::BadStart),
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')) {
            return Err(NameError::InvalidChar);
        }
    }
    Ok(())
}

/// Derive the SRI of a child named `local_name` under the namespace `parent`.
///
/// `parent` is the parent cell's SRI (its UUID). This is the single primitive
/// the spawn path uses; both the guest and the host call it with identical
/// inputs and therefore agree on the result.
pub fn child_sri(parent: Uuid, local_name: &str) -> Result<Uuid, NameError> {
    validate_segment(local_name)?;
    Ok(Uuid::new_v5(&parent, local_name.as_bytes()))
}

/// Resolve a full SRN path (e.g. `myapp/gateway/worker-1`) to its SRI by
/// folding UUIDv5 from [`ROOT_NS`] over each `/`-separated segment.
pub fn sri_of_path(path: &str) -> Result<Uuid, NameError> {
    let mut cur = ROOT_NS;
    for seg in path.split(SEP) {
        cur = child_sri(cur, seg)?;
    }
    Ok(cur)
}

/// Resolve a target string that is *either* a UUID literal *or* an SRN path.
///
/// This is the edge rule (CLI, gateway): if the string parses as a UUID it is
/// taken as an SRI verbatim; otherwise it is treated as an SRN path and
/// derived. The network only ever sees the resulting UUID.
pub fn resolve_target(s: &str) -> Result<Uuid, NameError> {
    match Uuid::try_parse(s) {
        Ok(uuid) => Ok(uuid),
        Err(_) => sri_of_path(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn root_ns_reads_as_intended() {
        // Locks the constant: if this literal changes, every identity moves.
        assert_eq!(ROOT_NS.to_string(), "a11ce115-dead-beef-cafe-c0ffee1decaf");
    }

    #[test]
    fn known_answer_vectors() {
        // Pinned absolute values. A change here means the derivation or ROOT_NS
        // drifted — every cell identity in the network would shift, so only
        // update these deliberately.
        assert_eq!(
            sri_of_path("myapp").unwrap().to_string(),
            "2dc85832-4752-5fcc-87fb-50df0d636319",
        );
    }

    #[test]
    fn path_fold_equals_manual_v5_fold() {
        let app = Uuid::new_v5(&ROOT_NS, b"myapp");
        let gw = Uuid::new_v5(&app, b"gateway");
        let worker = Uuid::new_v5(&gw, b"worker-1");
        assert_eq!(sri_of_path("myapp").unwrap(), app);
        assert_eq!(sri_of_path("myapp/gateway").unwrap(), gw);
        assert_eq!(sri_of_path("myapp/gateway/worker-1").unwrap(), worker);
    }

    #[test]
    fn child_of_parent_equals_full_path() {
        let gw = sri_of_path("myapp/gateway").unwrap();
        assert_eq!(
            child_sri(gw, "worker-1").unwrap(),
            sri_of_path("myapp/gateway/worker-1").unwrap(),
        );
    }

    #[test]
    fn grammar_rejects_bad_paths() {
        assert_eq!(sri_of_path(""), Err(NameError::EmptySegment));
        assert_eq!(sri_of_path("a//b"), Err(NameError::EmptySegment));
        assert_eq!(sri_of_path("a/"), Err(NameError::EmptySegment));
        assert_eq!(sri_of_path("/a"), Err(NameError::EmptySegment));
        assert_eq!(sri_of_path("App"), Err(NameError::BadStart));
        assert_eq!(child_sri(ROOT_NS, "-bad"), Err(NameError::BadStart));
        assert_eq!(sri_of_path("a b"), Err(NameError::InvalidChar));
    }

    #[test]
    fn resolve_target_accepts_uuid_or_path() {
        let sri = sri_of_path("myapp/gateway").unwrap();
        assert_eq!(resolve_target(&sri.to_string()).unwrap(), sri);
        assert_eq!(resolve_target("myapp/gateway").unwrap(), sri);
    }
}
