use introspection_client::v1::{Client, PluginInformation};

mod membership;
mod status;

async fn test_client() -> Client {
    // Join this test process's private multicast group so the client discovers
    // the swarms started via `swarm_config!` (which are isolated off the default
    // group), not a foreign process's swarm.
    let session = sorg_tests::test_session().await;
    Client::new(session).await
}

#[track_caller]
fn assert_plugin_configured(plugins: &[PluginInformation], name: &str) {
    let _plugin = plugins
        .iter()
        .find(|p_info| p_info.name == name)
        .unwrap_or_else(|| panic!("plugin not found: {}", name));
}
