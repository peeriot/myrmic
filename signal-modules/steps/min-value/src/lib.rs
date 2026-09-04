#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;

pub struct MinValueConfig;

pub struct MinValueState {
    min: f32,
}

impl MinValueState {
    pub fn new(_cfg: MinValueConfig) -> Self {
        Self { min: f32::INFINITY }
    }
}

impl ProcessingStep for MinValueState {
    type Input = f32;
    type Output = f32;

    fn step(&mut self, value: f32) -> Option<f32> {
        self.min = self.min.min(value);
        Some(self.min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_running_min() {
        let mut s = MinValueState::new(MinValueConfig);
        assert_eq!(s.step(3.0), Some(3.0));
        assert_eq!(s.step(5.0), Some(3.0));
        assert_eq!(s.step(1.0), Some(1.0));
        assert_eq!(s.step(2.0), Some(1.0));
    }

    #[test]
    fn handles_positives() {
        let mut s = MinValueState::new(MinValueConfig);
        assert_eq!(s.step(100.0), Some(100.0));
        assert_eq!(s.step(50.0), Some(50.0));
        assert_eq!(s.step(200.0), Some(50.0));
    }
}
