use std::time::Duration;

const DEFAULT_QUERY_DURATION: Duration = Duration::from_secs(15);

/// The configuration of the sorg client
#[derive(Default, Debug, Clone, Copy)]
pub struct Config {
    query_timeout: Option<Duration>,
}

impl Config {
    pub fn set_query_timeout(&mut self, query_timeout: Duration) {
        self.query_timeout = Some(query_timeout);
    }

    #[must_use]
    pub fn query_timeout(&self) -> Duration {
        self.query_timeout.unwrap_or(DEFAULT_QUERY_DURATION)
    }
}
