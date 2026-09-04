use rand::Rng;

#[derive(Copy, Clone, Debug)]
pub struct GeneratedTrace {
    trace_id: u128,
    span_id: u64,
}

impl GeneratedTrace {
    #[must_use]
    pub fn as_tuple(&self) -> (u128, u64) {
        (self.trace_id, self.span_id)
    }
}

impl Default for GeneratedTrace {
    fn default() -> Self {
        let mut rng = rand::rng();
        Self {
            trace_id: rng.random_range(1..u128::MAX),
            // we use NO_PARENT_SPAN_ID (0) here so all children of thie span context will
            // actually be a root span but uses the pre-generated the trace_id.
            span_id: crate::NO_PARENT_SPAN_ID,
        }
    }
}

impl core::fmt::Display for GeneratedTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", uuid::Uuid::from_u128(self.trace_id).as_simple())
    }
}
