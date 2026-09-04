use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    init_timeout_secs: Option<f64>,
}

type OrchConfig = sorg_orchestration::Config;

impl From<Config> for OrchConfig {
    fn from(value: Config) -> Self {
        let mut orch_config = OrchConfig::default();
        if let Some(init_timeout_secs) = value.init_timeout_secs {
            orch_config.set_init_timeout(Duration::from_secs_f64(init_timeout_secs));
        }
        orch_config
    }
}
