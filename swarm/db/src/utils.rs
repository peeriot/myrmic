pub use hashed::HashedBytes;
use std::str::FromStr;
pub use str_small::SmallString;

mod hashed;
mod str_small;

pub enum Either<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> Either<L, R> {
    pub fn reduce<U, FL, FR>(self, reduce_left: FL, reduce_right: FR) -> U
    where
        FL: FnOnce(L) -> U,
        FR: FnOnce(R) -> U,
    {
        match self {
            Either::Left(value) => reduce_left(value),
            Either::Right(value) => reduce_right(value),
        }
    }
}

pub fn try_small<F>(value: &str, mut hash: F) -> anyhow::Result<Either<SmallString, HashedBytes>>
where
    F: for<'a> FnMut(&'a str) -> anyhow::Result<HashedBytes>,
{
    if let Ok(smol) = SmallString::from_str(value) {
        Ok(Either::Left(smol))
    } else {
        Ok(Either::Right(hash(value)?))
    }
}

#[cfg(test)]
pub fn display_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\0' => out.push_str("\\0"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7E => out.push(b as char),
            _ => {
                use std::fmt::Write;
                let _ignore = write!(out, "\\x{:02X}", b);
            }
        }
    }
    out
}
