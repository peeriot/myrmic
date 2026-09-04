#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;
use signal_layer_types::ThresholdAlarm;

pub struct ThresholdTriggerConfig {
    pub threshold: f32,
    pub fire_below: bool,
}

/// Edge-triggered threshold node.
///
/// Emits exactly one [`ThresholdAlarm`] on every `inactive → active` transition
/// (i.e. when the input first enters the trigger region). Subsequent samples
/// that stay in the trigger region produce no event. Re-entering the trigger
/// region after leaving it emits a new alarm.
pub struct ThresholdTriggerState {
    threshold: f32,
    fire_below: bool,
    active: bool,
}

impl ThresholdTriggerState {
    pub fn new(cfg: ThresholdTriggerConfig) -> Self {
        Self {
            threshold: cfg.threshold,
            fire_below: cfg.fire_below,
            active: false,
        }
    }
}

impl ProcessingStep for ThresholdTriggerState {
    type Input = f32;
    type Output = ThresholdAlarm;

    fn step(&mut self, value: f32) -> Option<ThresholdAlarm> {
        let in_trigger_region = if self.fire_below {
            value < self.threshold
        } else {
            value > self.threshold
        };
        let edge = in_trigger_region && !self.active;
        self.active = in_trigger_region;
        if edge {
            Some(ThresholdAlarm {
                value,
                threshold: self.threshold,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_on_rising_edge() {
        let mut s = ThresholdTriggerState::new(ThresholdTriggerConfig {
            threshold: 50.0,
            fire_below: false,
        });
        assert!(s.step(49.0).is_none());
        assert!(s.step(50.0).is_none()); // equal does not fire
        let alarm = s.step(51.0).unwrap();
        assert_eq!(alarm.threshold, 50.0);
        assert!((alarm.value - 51.0).abs() < 1e-5, "value={}", alarm.value);
        // Subsequent samples above threshold must not re-fire.
        assert!(
            s.step(52.0).is_none(),
            "level-triggered re-fire above threshold"
        );
        assert!(
            s.step(60.0).is_none(),
            "level-triggered re-fire above threshold"
        );
    }

    #[test]
    fn fires_once_on_falling_edge() {
        let mut s = ThresholdTriggerState::new(ThresholdTriggerConfig {
            threshold: 10.0,
            fire_below: true,
        });
        assert!(s.step(10.0).is_none());
        assert!(s.step(11.0).is_none());
        let alarm = s.step(9.0).unwrap();
        assert!((alarm.value - 9.0).abs() < 1e-5, "value={}", alarm.value);
        // Staying below threshold must not re-fire.
        assert!(s.step(8.0).is_none());
        assert!(s.step(0.0).is_none());
    }

    #[test]
    fn re_enters_trigger_region_re_fires() {
        let mut s = ThresholdTriggerState::new(ThresholdTriggerConfig {
            threshold: 50.0,
            fire_below: false,
        });
        // Enter trigger region → fires.
        assert!(s.step(60.0).is_some());
        // Leave trigger region → no event.
        assert!(s.step(40.0).is_none());
        // Re-enter trigger region → fires again.
        assert!(s.step(70.0).is_some());
    }

    #[test]
    fn first_sample_in_trigger_region_fires() {
        // Edge from the initial inactive state still counts as a rising edge.
        let mut s = ThresholdTriggerState::new(ThresholdTriggerConfig {
            threshold: 0.0,
            fire_below: false,
        });
        assert!(s.step(1.0).is_some());
    }
}
