use std::time::Duration;

const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(15); // larger than the zenoh timeout, since we would prefer the timeout to come from the exec

#[derive(Clone, Default)]
pub struct Config {
    init_timeout: Option<Duration>,
}

impl Config {
    pub fn set_init_timeout(&mut self, init_timeout: Duration) -> &mut Self {
        self.init_timeout = Some(init_timeout);
        self
    }

    pub(crate) fn init_timeout(&self) -> Duration {
        self.init_timeout.unwrap_or(DEFAULT_INIT_TIMEOUT)
    }
}
