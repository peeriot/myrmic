//! Host-only unit tests for the TLS link layer (Level 1 — no hardware needed).
//!
//! # Running
//!
//! ```sh
//! cargo test -p zenoh-nano --features tls --test tls_link_test
//! ```

// ── TlsLink channel types compile ────────────────────────────────────────

/// Compile-time check that TlsLinkReceive/Send implement LinkReceive/LinkSend.
#[test]
fn tls_link_halves_implement_traits() {
    use zenoh_nano::link::tls::{TlsLinkReceive, TlsLinkSend};
    use zenoh_nano::link::{LinkReceive, LinkSend};

    fn assert_link_receive<T: LinkReceive>() {}
    fn assert_link_send<T: LinkSend>() {}

    assert_link_receive::<TlsLinkReceive<'_>>();
    assert_link_send::<TlsLinkSend<'_>>();
}

/// TLS_LINK_MTU + 2-byte framing header must fit inside TLS_BUF_SIZE.
#[test]
fn tls_link_mtu_value() {
    use zenoh_nano::link::tls::{TLS_BUF_SIZE, TLS_LINK_MTU};

    assert!(
        (TLS_LINK_MTU as usize + 2) <= TLS_BUF_SIZE,
        "TLS_LINK_MTU + framing ({}) must fit in TLS_BUF_SIZE ({})",
        TLS_LINK_MTU as usize + 2,
        TLS_BUF_SIZE,
    );
}

/// MaxFragmentLength::Bits9 allows 512 B plaintext; minus 2-byte frame header = 510.
#[test]
fn tls_link_mtu_matches_bits9_plaintext_limit() {
    use zenoh_nano::link::tls::TLS_LINK_MTU;

    const TLS_BITS9_PLAINTEXT_LIMIT: usize = 512;
    const FRAME_HEADER_SIZE: usize = 2;
    assert_eq!(
        TLS_LINK_MTU as usize + FRAME_HEADER_SIZE,
        TLS_BITS9_PLAINTEXT_LIMIT,
        "TLS_LINK_MTU + framing must match the Bits9 plaintext limit",
    );
}
