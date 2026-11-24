//! Module for the topics which are known exclusively to orch plugins

use zenoh::config::ZenohId;

pub(crate) const TOPIC_ORCH_STATE_UPDATE: &str = "@sorg/@orch/@state-update";

/// Key prefix shared by the liveliness tokens of all orchs
pub(crate) const TOPIC_ORCH_LIVELINESS_PREFIX: &str = "@sorg/@orch/";

/// Key expression matching the liveliness token of every orch
pub(crate) const TOPIC_ORCH_LIVELINESS_ALL: &str = "@sorg/@orch/*";

/// Key of the liveliness token an orch keeps alive for as long as it runs
pub(crate) fn topic_orch_liveliness(zid: ZenohId) -> String {
    format!("{TOPIC_ORCH_LIVELINESS_PREFIX}{zid}")
}
