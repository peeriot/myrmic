use filestore_client::Client;
use swarm::spawn::Spawned;

mod basic;

struct IsolatedSwarm {
    handle: Spawned,
    client: Client,
}

async fn set_up_client() -> Client {
    let mut zenoh_config = zenoh::Config::default();
    zenoh_config
        .insert_json5("mode", r#""peer""#)
        .expect("zenoh mode");
    zenoh_config
        .insert_json5("scouting/multicast/enabled", "false")
        .expect("zenoh multicast");
    zenoh_config
        .insert_json5("scouting/gossip/enabled", "false")
        .expect("zenoh gossip");
    let session = zenoh::open(zenoh_config)
        .await
        .expect("failed to open zenoh session");

    Client::new(&session)
}

async fn set_up_file_store() -> IsolatedSwarm {
    set_up_swarm(true).await
}

async fn set_up_swarm_without_file_store() -> IsolatedSwarm {
    set_up_swarm(false).await
}

async fn set_up_swarm(with_file_store: bool) -> IsolatedSwarm {
    let listen_endpoint = "tcp/127.0.0.1:0";
    let plugin_config = if with_file_store { "db: {}" } else { "" };

    let config = format!(
        r#"
local z = import "zenoh.libsonnet";

z.peer()
+ z.plugins.dev({{
  {plugin_config}
}})
+ {{
  zenoh+: {{
    listen+: {{
      endpoints: {{
        peer: ["{listen_endpoint}"],
      }},
    }},
    scouting+: {{
      multicast+: {{
        enabled: false,
      }},
      gossip+: {{
        enabled: false,
      }},
    }},
    open+: {{
      return_conditions+: {{
        connect_scouted: true,
        declares: true,
      }},
    }},
  }},
}}
"#
    );

    let handle = swarm::Swarm::parse(config)
        .expect("Unable to configure swarm")
        .wait_in_place()
        .await
        .expect("Unable to spawn swarm");
    let endpoint = bound_endpoint(handle.session()).await;
    let client = connect_client(&endpoint).await;

    IsolatedSwarm { handle, client }
}

async fn bound_endpoint(session: &zenoh::Session) -> String {
    session
        .info()
        .locators()
        .await
        .into_iter()
        .map(|locator| locator.to_string())
        .find(|locator| locator.starts_with("tcp/127.0.0.1:"))
        .expect("isolated swarm did not expose its localhost TCP listener")
}

async fn connect_client(endpoint: &str) -> Client {
    let session = try_open_client(endpoint)
        .await
        .unwrap_or_else(|| panic!("timed out connecting to isolated swarm at {endpoint}"));

    Client::new(&session)
}

async fn try_open_client(endpoint: &str) -> Option<zenoh::Session> {
    for _ in 0..50 {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("mode", r#""client""#)
            .expect("zenoh mode");
        config
            .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
            .expect("zenoh endpoints");
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .expect("zenoh multicast");
        config
            .insert_json5("scouting/gossip/enabled", "false")
            .expect("zenoh gossip");
        config
            .insert_json5("open/return_conditions/connect_scouted", "true")
            .expect("zenoh connect_scouted");
        config
            .insert_json5("open/return_conditions/declares", "true")
            .expect("zenoh declares");

        if let Ok(Ok(session)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), zenoh::open(config)).await
        {
            return Some(session);
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    None
}
