//! Symbols referenced by `import!`-generated code.
//!
//! The `codegen` module in `myrmic-common` rewrites the absolute paths in
//! `typify`'s output so they resolve through `exports`. That keeps a generated
//! cell self-contained: it needs to depend on `myrmic-sdk` only — never directly on `serde`,
//! `serde_json`, or an `alloc`/`std` prelude.
//!
//! Everything here is `#[doc(hidden)]`: it is an implementation detail of the
//! `import!` macro, not public API.

pub mod exports {
    #![allow(unused_imports)]

    pub use ::serde;
    pub use ::serde_json;
    // Formats typify maps to external crates: `format: uuid` -> uuid::Uuid,
    // `format: date-time`/`date` -> chrono::{DateTime, NaiveDate, …}.
    pub use ::chrono;
    pub use ::uuid;

    /// `typify` emits `::std::…` paths; re-map each segment it uses to the
    /// `core`/`alloc` equivalent so the generated code needs no `std`.
    pub mod std {
        pub use ::alloc::{borrow, boxed, collections, rc, string, sync, vec};
        pub use ::core::{
            clone, cmp, convert, default, error, fmt, marker, ops, option, result, str,
        };
    }
}
