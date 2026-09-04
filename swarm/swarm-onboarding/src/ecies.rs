//! This module contains a simple implementation of the ECIES symmetric key material derivation.
//!
//! Based on `RustCrypto`

use aes_gcm::{Aes256Gcm, Key};

use elliptic_curve::ecdh::{EphemeralSecret, SharedSecret, diffie_hellman};
use elliptic_curve::point::PointCompression;
use elliptic_curve::rand_core::CryptoRngCore;
use elliptic_curve::sec1::{EncodedPoint, FromEncodedPoint, ModulusSize, ToEncodedPoint};
use elliptic_curve::{Curve, CurveArithmetic, PublicKey, SecretKey};

use embedded_io_async::ErrorKind;

use sha2::Sha256;

use crate::utils::base64::{base64_decode, base64_encode};

pub mod io;

/// An enum representing either an ephemeral secret or a static secret key for ECIES key derivation.
///
/// The ephemeral secret case is expected to be used by the Installer during the onboarding process,
/// while the static secret key case is expected to be used by the Device.
pub enum Secret<C: CurveArithmetic> {
    Ephemeral(EphemeralSecret<C>),
    SecretKey(SecretKey<C>),
}

impl<C: CurveArithmetic> Secret<C>
where
    C: PointCompression,
    <C as Curve>::FieldBytesSize: ModulusSize,
    <C as CurveArithmetic>::AffinePoint: FromEncodedPoint<C>,
    <C as CurveArithmetic>::AffinePoint: ToEncodedPoint<C>,
{
    /// Create a new ephemeral secret key.
    ///
    /// # Arguments
    /// - `rng`: A cryptographically secure random number generator.
    pub fn new_ephemeral<R: CryptoRngCore>(rng: &mut R) -> Self {
        Self::Ephemeral(EphemeralSecret::random(rng))
    }

    /// Create a new secret key from the given private key bytes in SEC1 DER format.
    ///
    /// # Arguments
    /// - `private_key`: The private key bytes in SEC1 DER format.
    pub fn new_secret(private_key: &[u8]) -> Self {
        Self::SecretKey(SecretKey::from_sec1_der/*from_slice*/(private_key).unwrap())
    }

    /// Extract the public key in base64-encoded SEC1 format.
    ///
    /// # Arguments
    /// - `buf`: A mutable buffer to store the base64-encoded public key.
    ///
    /// # Returns
    /// - `Ok((pub_key_str, remaining_buf))`: The base64-encoded public key string and the remaining buffer space.
    /// - `Err(ErrorKind::OutOfMemory)`: The buffer was not large enough to hold the base64-encoded public key.
    pub fn extract_pub_key<'a>(
        &self,
        buf: &'a mut [u8],
    ) -> Result<(&'a str, &'a mut [u8]), ErrorKind> {
        let pub_key = self.public_key();

        let (pub_key_str, remaining_buf) =
            base64_encode(EncodedPoint::<C>::from(pub_key).as_bytes(), buf)
                .map_err(|_| ErrorKind::OutOfMemory)?;

        Ok((pub_key_str, remaining_buf))
    }

    /// Derive a symmetric AES256-GCM key from the given peer public key in base64-encoded SEC1 format.
    ///
    /// # Arguments
    /// - `peer_pub_key_base64`: The peer's public key in base64-encoded SEC1 format.
    /// - `buf`: A mutable buffer to decode the base64-encoded public key.
    ///
    /// # Returns
    /// - `Ok(Some(key))`: The derived AES256-GCM key if the peer public key was provided.
    /// - `Ok(None)`: If no peer public key was provided.
    /// - `Err(ErrorKind::OutOfMemory)`: The buffer was not large enough to hold the decoded public key.
    pub fn derive_crypto_key(
        &self,
        peer_pub_key_base64: Option<&str>,
        buf: &mut [u8],
    ) -> Result<Option<Key<Aes256Gcm>>, ErrorKind> {
        if let Some(peer_pub_key) = peer_pub_key_base64 {
            let (peer_pub_key, _) =
                base64_decode(peer_pub_key, buf).map_err(|_| ErrorKind::OutOfMemory)?;

            let key = self.compute_crypto_key(peer_pub_key);

            Ok(Some(key))
        } else {
            Ok(None)
        }
    }

    /// Compute the shared secret and derive a symmetric AES256-GCM key using HKDF-SHA256.
    ///
    /// # Arguments
    /// - `rng`: A cryptographically secure random number generator.
    /// - `peer_public_key`: The peer's public key in SEC1 encoded format.
    ///
    /// # Returns
    /// - A tuple containing the ephemeral public key in SEC1 encoded format and the derived AES256-GCM key.
    #[allow(unused)]
    pub fn compute_crypto_key(&self, peer_pub_key: &[u8]) -> Key<Aes256Gcm> {
        let peer_pub_key = PublicKey::from_sec1_bytes(peer_pub_key).unwrap(); // TODO

        let hkdf = self
            .compute_shared_secret(&peer_pub_key)
            .extract::<Sha256>(None);

        let mut okm = [0; 32];
        hkdf.expand(&[], &mut okm).unwrap();

        okm.into()
    }

    /// Compute the shared secret using ECDH
    ///
    /// # Arguments
    /// - `peer_public_key`: The peer's public key.
    ///
    /// # Returns
    /// - A tuple containing the shared secret and the ephemeral public key.
    fn compute_shared_secret(&self, peer_public_key: &PublicKey<C>) -> SharedSecret<C> {
        match self {
            Self::Ephemeral(ephemeral_secret) => ephemeral_secret.diffie_hellman(peer_public_key),
            Self::SecretKey(secret_key) => {
                diffie_hellman(secret_key.to_nonzero_scalar(), peer_public_key.as_affine())
            }
        }
    }

    /// Get the public key corresponding to the secret.
    fn public_key(&self) -> PublicKey<C> {
        match self {
            Self::Ephemeral(ephemeral_secret) => ephemeral_secret.public_key(),
            Self::SecretKey(secret_key) => secret_key.public_key(),
        }
    }
}
