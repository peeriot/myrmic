#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;

const MAX_WINDOW: usize = 64;

#[derive(Clone, Copy)]
pub struct MovingAverageConfig {
    pub window: usize,
}

impl Default for MovingAverageConfig {
    fn default() -> Self {
        Self { window: 8 }
    }
}

pub struct MovingAverageState {
    buf: [f32; MAX_WINDOW],
    idx: usize,
    count: usize,
    window: usize,
}

impl MovingAverageState {
    pub fn new(cfg: MovingAverageConfig) -> Self {
        let window = cfg.window.clamp(1, MAX_WINDOW);
        Self {
            buf: [0.0; MAX_WINDOW],
            idx: 0,
            count: 0,
            window,
        }
    }
}

impl ProcessingStep for MovingAverageState {
    type Input = f32;
    type Output = f32;

    fn step(&mut self, input: f32) -> Option<f32> {
        self.buf[self.idx] = input;
        self.idx = (self.idx + 1) % self.window;
        if self.count < self.window {
            self.count += 1;
        }
        if self.count == self.window {
            let sum: f32 = self.buf[..self.window].iter().sum();
            // `window` is clamped to `1..=MAX_WINDOW` (64), well inside f32's
            // exactly-representable integer range.
            #[expect(
                clippy::cast_precision_loss,
                reason = "window is at most MAX_WINDOW (64)"
            )]
            Some(sum / self.window as f32)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_fills_then_emits() {
        let mut s = MovingAverageState::new(MovingAverageConfig { window: 3 });
        assert!(s.step(1.0).is_none());
        assert!(s.step(2.0).is_none());
        let avg = s.step(3.0).unwrap();
        assert!((avg - 2.0).abs() < 1e-5, "avg={avg}");
    }

    #[test]
    fn sliding_window_updates() {
        let mut s = MovingAverageState::new(MovingAverageConfig { window: 3 });
        s.step(1.0);
        s.step(2.0);
        s.step(3.0);
        // ring: buf=[1,2,3], idx=0 after 3 steps
        // step 4: buf[0]=6 → buf=[6,2,3] → avg = 11/3
        let avg = s.step(6.0).unwrap();
        assert!((avg - 11.0 / 3.0).abs() < 1e-4, "avg={avg}");
    }

    #[test]
    fn window_1_always_emits() {
        let mut s = MovingAverageState::new(MovingAverageConfig { window: 1 });
        assert_eq!(s.step(5.0), Some(5.0));
        assert_eq!(s.step(7.0), Some(7.0));
    }
}
