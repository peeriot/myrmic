mod event_loop;
mod queryables;
mod spawn;

pub(crate) type Result<T, E = test_control_common::Error> = core::result::Result<T, E>;

pub use spawn::spawn;

pub(crate) use event_loop::Event;
