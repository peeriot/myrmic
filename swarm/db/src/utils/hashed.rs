use siphasher::sip128::{Hasher128, SipHasher24};
use std::hash::Hasher;

#[derive(Eq, PartialEq, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct HashedBytes([u8; 16]);

impl HashedBytes {
    pub fn new<B: AsRef<[u8]>>(value: B) -> Self {
        let mut hasher = SipHasher24::new();
        hasher.write(value.as_ref());
        let hash = hasher.finish128().as_u128().to_be_bytes();
        Self(hash)
    }

    #[inline]
    pub fn from_be_bytes(hash: [u8; 16]) -> Self {
        Self(hash)
    }

    #[inline]
    pub fn to_be_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl std::hash::Hash for HashedBytes {
    #[inline]
    #[expect(
        clippy::host_endian_bytes,
        reason = "process-local in-memory hash only, so native-endian byte order is correct"
    )]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u128(u128::from_ne_bytes(self.0));
    }
}

impl std::fmt::Debug for HashedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self, f)
    }
}

impl std::fmt::Display for HashedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, byte) in self.0.into_iter().enumerate() {
            write!(f, "{:0>2X}", byte)?;
            if i != self.0.len() - 1 {
                write!(f, "_")?;
            }
        }
        Ok(())
    }
}
