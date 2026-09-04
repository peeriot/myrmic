use embedded_tls::{
    Aes128GcmSha256, CryptoProvider, NoClock, SignatureScheme, TlsError, TlsVerifier,
    UnsecureProvider, pki::CertVerifier,
};
use rand_core::CryptoRngCore;

/// TLS crypto provider for ESP32 mTLS.
///
/// - Signer: ECDSA/NistP256 via `UnsecureProvider` (client certificate auth).
/// - Verifier: CA-chain validation via `CertVerifier` with `NoClock`.
///
/// # NoClock — cert validity not checked
///
/// `NoClock` is intentional: the ESP32 has no RTC and no SNTP sync at TLS
/// handshake time, so `notBefore`/`notAfter` fields cannot be validated.
/// CA chain and signature verification are still enforced; only time-based
/// validity is skipped.  Ensure short-lived or pinned certificates are used
/// in production to bound exposure from this limitation.
pub struct CaCheckingProvider<R>
where
    R: CryptoRngCore,
{
    signer: UnsecureProvider<Aes128GcmSha256, R>,
    verifier: CertVerifier<Aes128GcmSha256, NoClock, 4096>,
}

impl<R> CaCheckingProvider<R>
where
    R: CryptoRngCore,
{
    #[must_use]
    pub fn new(rng: R) -> Self {
        Self {
            signer: UnsecureProvider::new::<Aes128GcmSha256>(rng),
            verifier: CertVerifier::new(),
        }
    }
}

impl<R> CryptoProvider for CaCheckingProvider<R>
where
    R: CryptoRngCore,
{
    type CipherSuite = Aes128GcmSha256;
    type Signature = <UnsecureProvider<Aes128GcmSha256, R> as CryptoProvider>::Signature;

    fn rng(&mut self) -> impl CryptoRngCore {
        self.signer.rng()
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }

    fn signer(
        &mut self,
        key_der: &[u8],
    ) -> Result<(impl signature::SignerMut<Self::Signature>, SignatureScheme), TlsError> {
        self.signer.signer(key_der)
    }
}
