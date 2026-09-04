use std::borrow::Borrow;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::str::{FromStr, Utf8Error};

/// Backing array size. Index `LEN_IDX` holds the stored length; the remaining
/// `MAX_LEN` bytes hold the string data.
const SIZE: usize = 16;
/// Index of the byte that stores the current length.
const LEN_IDX: usize = SIZE - 1;
/// Maximum number of data bytes (everything except the length byte).
const MAX_LEN: usize = SIZE - 1;

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SmallString([u8; SIZE]);

impl SmallString {
    #[inline]
    pub const fn new() -> Self {
        Self([0; SIZE])
    }

    #[inline]
    pub fn from_utf8(bytes: &[u8]) -> Result<Self, Error> {
        Self::from_str(str::from_utf8(bytes).map_err(Error::BadUtf8)?)
    }

    #[inline]
    pub fn from_be_bytes(bytes: [u8; SIZE]) -> Result<Self, Error> {
        // The stored length byte is untrusted here (`bytes` may come straight from a
        // deserialized/persisted value), so bound it against the data capacity before
        // using it to slice - otherwise a corrupt length would panic.
        let len = usize::from(bytes[LEN_IDX]);
        if len > MAX_LEN {
            return Err(Error::TooLong(len));
        }
        // We check that it is valid UTF-8
        str::from_utf8(&bytes.as_ref()[..len]).map_err(Error::BadUtf8)?;
        Ok(Self(bytes))
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0[LEN_IDX].into()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    #[expect(
        clippy::expect_used,
        reason = "every constructor validates UTF-8, so the stored bytes are valid by construction"
    )]
    pub fn as_str(&self) -> &str {
        str::from_utf8(self.as_bytes()).expect("unable to convert smallstring to utf8")
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..self.len()]
    }

    #[inline]
    pub fn to_be_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl Deref for SmallString {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for SmallString {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for SmallString {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Debug for SmallString {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl std::fmt::Display for SmallString {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for SmallString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SmallString {}

impl PartialOrd for SmallString {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmallString {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for SmallString {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<SmallString> for String {
    #[inline]
    fn from(value: SmallString) -> Self {
        value.as_str().into()
    }
}

impl<'a> From<&'a SmallString> for &'a str {
    #[inline]
    fn from(value: &'a SmallString) -> Self {
        value.as_str()
    }
}

impl FromStr for SmallString {
    type Err = Error;

    #[inline]
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() <= MAX_LEN {
            let mut inner = [0; SIZE];
            inner[..value.len()].copy_from_slice(value.as_bytes());
            // `value.len() <= MAX_LEN < u8::MAX`, so this conversion cannot fail; the
            // map_err only re-labels the (data-less) TryFromIntError as the meaningful
            // TooLong error.
            #[expect(
                clippy::map_err_ignore,
                reason = "TryFromIntError carries no data; unreachable here given the length guard"
            )]
            {
                inner[LEN_IDX] = value
                    .len()
                    .try_into()
                    .map_err(|_| Self::Err::TooLong(value.len()))?;
            }
            Ok(Self(inner))
        } else {
            Err(Self::Err::TooLong(value.len()))
        }
    }
}

impl<'a> TryFrom<&'a str> for SmallString {
    type Error = Error;

    #[inline]
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        Self::from_str(value)
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum Error {
    #[error("small strings could only contain at most 15 characters, found {0}")]
    TooLong(usize),
    #[error(transparent)]
    BadUtf8(#[from] Utf8Error),
}

#[cfg(test)]
mod tests {
    use super::SmallString;

    #[test]
    fn basic_functionality() {
        let a = SmallString::try_from("abcdefghijklmno").expect("Should be able to store");
        let bytes = a.to_be_bytes();
        let b = SmallString::from_be_bytes(bytes).expect("Should be able to reconstruct string");

        assert_eq!(a, b);
    }
}
