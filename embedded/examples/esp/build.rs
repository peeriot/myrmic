use std::path::Path;

fn main() {
    // These cert artifacts are embedded via include_bytes! in the ESP mTLS examples.
    // They live outside this crate, so we explicitly declare them as inputs.
    let cert_paths = [
        "../../../tests/integration/certs/ca.der",
        "../../../tests/integration/certs/esp.der",
        "../../../tests/integration/certs/esp.key.der",
        "../../../tests/integration/certs/ca.crt",
        "../../../tests/integration/certs/laptop.crt",
        "../../../tests/integration/certs/esp.crt",
        "../../../tests/integration/certs/laptop.key",
        "../../../tests/integration/certs/esp.key",
    ];

    for rel in cert_paths {
        // Emit unconditionally so cargo re-runs this script when a cert is
        // generated for the first time (not just when it changes).
        println!("cargo:rerun-if-changed={}", Path::new(rel).display());
    }
}
