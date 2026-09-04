//! The child-side fencing decision core lives in `cell_protocol::supervision`
//! so the embedded cell host runs the exact same logic; this exec is one of
//! its drivers (evidence gathering in `event_loop/supervision_pass.rs`).

pub(crate) use cell_protocol::supervision::{
    Evidence, FencingState, RowFacts, RowRead, Verdict, WatchedCell,
};
