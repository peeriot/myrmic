#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;

pub struct MaxValueConfig;

pub struct MaxValueState {
    max: f32,
}

impl MaxValueState {
    pub fn new(_cfg: MaxValueConfig) -> Self {
        Self {
            max: f32::NEG_INFINITY,
        }
    }
}

impl ProcessingStep for MaxValueState {
    type Input = f32;
    type Output = f32;

    fn step(&mut self, value: f32) -> Option<f32> {
        self.max = self.max.max(value);
        Some(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_running_max() {
        let mut s = MaxValueState::new(MaxValueConfig);
        assert_eq!(s.step(3.0), Some(3.0));
        assert_eq!(s.step(1.0), Some(3.0));
        assert_eq!(s.step(5.0), Some(5.0));
        assert_eq!(s.step(4.0), Some(5.0));
    }

    #[test]
    fn handles_negatives() {
        let mut s = MaxValueState::new(MaxValueConfig);
        assert_eq!(s.step(-5.0), Some(-5.0));
        assert_eq!(s.step(-3.0), Some(-3.0));
        assert_eq!(s.step(-10.0), Some(-3.0));
    }
}
