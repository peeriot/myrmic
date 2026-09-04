/// Marker trait for event payload types.
///
/// Implemented automatically by the `#[event]` proc macro attribute.
pub trait CellEvent {
    #[doc(hidden)]
    fn event_name() -> &'static str;
}
