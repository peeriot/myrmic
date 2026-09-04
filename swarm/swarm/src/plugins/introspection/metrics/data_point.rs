#[derive(Debug)]
/// Returned when a metric that is expected to be monotonically increasing decreases.
///
/// This is currently used for cumulative process I/O counters, where a decrease would indicate
/// either bad source data or a semantic mismatch in how the metric is modeled.
pub(crate) struct MonotonicError;

#[derive(Default, Clone, Debug)]
/// Holds the latest sampled value of type `V` together with the delta to the previous sample as
/// type `O`.
///
/// `V` and `O` are usually the same type, but they can differ. An unsigned `O` enforces
/// monotonically increasing values because the offset cannot be negative. When non-monotonic data
/// is stored as an unsigned `V` (e.g. used memory, which can decrease), `O` should be a signed
/// type so the delta can represent both growth and shrinkage.
///
/// Each concrete type combination requires its own `update_value` implementation that handles the
/// monotonicity semantics appropriately.
///
/// The combinations used in this module have the following semantics:
/// - [`DataPoint<u64, u64>`] — cumulative counters; rejects decreases with [`MonotonicError`].
/// - [`DataPoint<u64, i64>`] — integer gauges; absolute value is unsigned, delta may be negative.
/// - [`DataPoint<f32, f32>`] — floating-point gauges with signed deltas.
pub(crate) struct DataPoint<V, O = V>
where
    V: Clone + Default,
    O: Clone,
{
    current_value: V,
    offset_to_previous: O,
}

pub(crate) type GaugeF32 = DataPoint<f32>;
pub(crate) type NonMonotonicCounter = DataPoint<u64, i64>;
pub(crate) type MonotonicCounter = DataPoint<u64>;

// unused until real publisher is implemented
#[allow(unused)]
impl<V: Default + Copy, O: Copy> DataPoint<V, O> {
    /// Returns the latest sampled value.
    pub(crate) fn value(&self) -> V {
        self.current_value
    }

    /// Returns the delta from the previous sample to the current one.
    ///
    /// The meaning of the delta depends on the concrete specialization:
    /// it may be signed for gauges or unsigned for monotonic counters.
    pub(crate) fn last_offset(&self) -> O {
        self.offset_to_previous
    }
}

/// Updates an unsigned gauge and stores a signed delta to the previous sample.
///
/// This never fails. Decreases are represented as negative offsets.
impl DataPoint<u64, i64> {
    /// Records `value` and updates the signed delta from the previous sample.
    pub(crate) fn update_value(&mut self, value: u64) {
        let diff = value
            .abs_diff(self.current_value)
            .min(i64::MAX.try_into().expect("i64::MAX fits u64"));

        if value > self.current_value {
            self.offset_to_previous = diff.cast_signed();
        } else {
            self.offset_to_previous = -(diff.cast_signed());
        }
        self.current_value = value;
    }
}

/// Updates a cumulative counter that is expected to be monotonically increasing.
///
/// On success, both the current value and the offset are updated.
/// On failure, the previous state is preserved.
impl DataPoint<u64, u64> {
    pub(crate) fn update_value(&mut self, value: u64) -> Result<(), MonotonicError> {
        if let Some(offset) = value.checked_sub(self.current_value) {
            self.offset_to_previous = offset;
            self.current_value = value;

            Ok(())
        } else {
            Err(MonotonicError)
        }
    }
}

/// Updates a floating-point gauge and stores the signed delta to the previous sample.
///
/// This never fails.
impl DataPoint<f32, f32> {
    /// Records `value` and updates the signed delta from the previous sample.
    pub(crate) fn update_value(&mut self, value: f32) {
        self.offset_to_previous = value - self.current_value;
        self.current_value = value;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn non_monotonic_data_point() {
        let mut dp = super::DataPoint::<u64, i64>::default();

        assert_eq!(0, dp.value());
        assert_eq!(0, dp.last_offset());

        dp.update_value(10);

        assert_eq!(10, dp.value());
        assert_eq!(10, dp.last_offset());

        dp.update_value(15);

        assert_eq!(15, dp.value());
        assert_eq!(5, dp.last_offset());

        dp.update_value(10);

        assert_eq!(10, dp.value());
        assert_eq!(-5, dp.last_offset());
    }

    #[test]
    fn monotonically_increasing_data_point() {
        let mut dp = super::DataPoint::<u64>::default();

        assert_eq!(0, dp.value());
        assert_eq!(0, dp.last_offset());

        dp.update_value(10).unwrap();

        assert_eq!(10, dp.value());
        assert_eq!(10, dp.last_offset());

        dp.update_value(15).unwrap();

        assert_eq!(15, dp.value());
        assert_eq!(5, dp.last_offset());

        // decrease will fail and keep previous values
        assert!(dp.update_value(10).is_err());

        assert_eq!(15, dp.value());
        assert_eq!(5, dp.last_offset());
    }
}
