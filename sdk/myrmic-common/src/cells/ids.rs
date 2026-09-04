//! Typed cell identities: [`Sri`] (the UUID identifier) and [`Srn`] (the
//! human-readable name). The derivation primitives live in [`super::naming`];
//! these are the newtypes the rest of the system passes around.

use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::naming::{self, NameError};

/// A cell's identity: the UUIDv5 folded from its [`Srn`] path (see
/// [`super::naming`]). This is the identifier the network, DB scopes, and
/// mailboxes route on — never the human name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Sri(Uuid);

impl Sri {
    /// The nil SRI — "no cell" (e.g. a message originating outside any cell).
    pub const NIL: Self = Self(Uuid::nil());

    /// Wraps a raw [`Uuid`] as an SRI. `const` so well-known targets whose SRI
    /// was derived offline can be baked in as constants.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Recombines the `(hi, lo)` halves the host splits an SRI into across the
    /// Wasm ABI.
    #[must_use]
    pub fn from_parts(hi: i64, lo: i64) -> Self {
        Self(Uuid::from_u64_pair(hi as u64, lo as u64))
    }

    /// Splits the SRI into the `(hi, lo)` i64 halves passed across the Wasm
    /// ABI. Inverse of [`Sri::from_parts`].
    #[must_use]
    pub fn to_parts(&self) -> (i64, i64) {
        let (hi, lo) = self.0.as_u64_pair();
        (hi as i64, lo as i64)
    }

    /// Splits the SRI into the `(hi, lo)` i64 halves passed across the Wasm ABI.
    /// Inverse of [`Sri::from_parts`].
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn as_parts(&self) -> (i64, i64) {
        let n = self.0.as_u128();

        ((n >> 64) as i64, n as i64)
    }

    /// Builds an SRI from raw 16 bytes — e.g. what a `spawn` host call writes
    /// back.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Uuid::from_bytes(bytes))
    }

    /// The raw 16 bytes of the underlying UUID.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 16] {
        *self.0.as_bytes()
    }

    /// The underlying [`Uuid`].
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Whether this is the nil SRI ("no cell").
    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// Derives the SRI of a full SRN path (e.g. `"myapp/gateway"`). Pure and
    /// offline — see [`super::naming::sri_of_path`].
    pub fn of_path(path: &str) -> Result<Self, NameError> {
        naming::sri_of_path(path).map(Self)
    }

    /// Derives the SRI of an [`Srn`].
    pub fn of_srn(srn: &Srn) -> Result<Self, NameError> {
        Self::of_path(srn.as_str())
    }

    /// The SRI of a child named `local_name` under this cell — the same value
    /// the host assigns when this cell spawns `local_name`.
    pub fn child(&self, local_name: &str) -> Result<Self, NameError> {
        naming::child_sri(self.0, local_name).map(Self)
    }

    /// The edge rule (CLI, gateway): if `s` parses as a UUID it is taken as an
    /// SRI verbatim; otherwise it is treated as an SRN path and derived.
    pub fn from_target(s: &str) -> Result<Self, NameError> {
        naming::resolve_target(s).map(Self)
    }
}

impl From<Uuid> for Sri {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<Sri> for Uuid {
    fn from(sri: Sri) -> Self {
        sri.0
    }
}

impl fmt::Display for Sri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for Sri {
    type Err = uuid::Error;

    /// Strict: parses a canonical UUID string only. For the "UUID-or-name" edge
    /// rule use [`Sri::from_target`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::from_str(s).map(Self)
    }
}

/// A cell's human-readable name: a `/`-separated path such as
/// `myapp/gateway/worker-1`. Lives only at the edges (CLI, config, registry
/// display); resolve it to an [`Sri`] before routing.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Srn(String);

impl Srn {
    /// Wraps a string as an SRN without validating the grammar. Use for names
    /// already known to be well-formed; prefer [`Srn::parse`] otherwise.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Validates the path grammar (each `/`-separated segment must match
    /// `^[a-z0-9][a-z0-9._-]*$`) and wraps it.
    pub fn parse(name: impl Into<String>) -> Result<Self, NameError> {
        let name = name.into();
        // Fold-derive purely to validate; discard the SRI.
        naming::sri_of_path(&name)?;
        Ok(Self(name))
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derives this name's [`Sri`].
    pub fn resolve(&self) -> Result<Sri, NameError> {
        Sri::of_path(&self.0)
    }
}

impl fmt::Display for Srn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Srn {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for Srn {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::{Sri, Srn};
    use crate::cells::naming;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    // The pinned SRI of "myapp" from naming.rs's known-answer vectors.
    const MYAPP_SRI: &str = "2dc85832-4752-5fcc-87fb-50df0d636319";

    #[test]
    fn sri_of_path_matches_naming_derivation() {
        let sri = Sri::of_path("myapp").unwrap();
        assert_eq!(sri.to_string(), MYAPP_SRI);
        assert_eq!(sri.as_uuid(), naming::sri_of_path("myapp").unwrap());
    }

    #[test]
    fn sri_display_parse_roundtrips() {
        let sri = Sri::of_path("myapp/gateway").unwrap();
        let parsed: Sri = sri.to_string().parse().unwrap();
        assert_eq!(sri, parsed);
    }

    #[test]
    fn sri_from_target_accepts_uuid_literal() {
        let sri = Sri::of_path("myapp/gateway").unwrap();
        assert_eq!(Sri::from_target(&sri.to_string()).unwrap(), sri);
    }

    #[test]
    fn sri_from_target_accepts_srn_path() {
        let sri = Sri::of_path("myapp/gateway").unwrap();
        assert_eq!(Sri::from_target("myapp/gateway").unwrap(), sri);
    }

    #[test]
    fn sri_child_equals_full_path() {
        let gw = Sri::of_path("myapp/gateway").unwrap();
        assert_eq!(
            gw.child("worker-1").unwrap(),
            Sri::of_path("myapp/gateway/worker-1").unwrap(),
        );
    }

    #[test]
    fn sri_postcard_roundtrips_as_compact_binary() {
        let sri = Sri::of_path("myapp/gateway").unwrap();
        let bytes: Vec<u8> = postcard::to_allocvec(&sri).unwrap();
        // Binary form, not the 37-byte length-prefixed hyphenated string.
        assert!(
            bytes.len() <= 17,
            "sri postcard encoding = {} bytes",
            bytes.len()
        );
        let back: Sri = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(sri, back);
    }

    #[test]
    fn srn_parse_rejects_invalid_paths() {
        assert!(Srn::parse("App").is_err());
        assert!(Srn::parse("a//b").is_err());
        assert!(Srn::parse("").is_err());
    }

    #[test]
    fn srn_resolves_to_same_sri_as_path() {
        let srn = Srn::parse("myapp/gateway").unwrap();
        assert_eq!(
            srn.resolve().unwrap(),
            Sri::of_path("myapp/gateway").unwrap()
        );
    }
}
