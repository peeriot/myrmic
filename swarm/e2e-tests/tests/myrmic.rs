use allure_cargotest::{allure_test, step};
use test_framework::{
    clients::{
        db::{DbHandle, Scope},
        sorg::SorgHandle,
    },
    mqtt::MqttBroker,
    myrmic::{LocalBinary, Myrmic, MyrmicBackend},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[allure_test]
#[tokio::test(flavor = "multi_thread")]
async fn myrmic_e2e() {
    // init myrmic backend for all tests
    let myrmic = Myrmic::local();

    // run tests
    runtime_management(&myrmic).await;
    deploy_cell(&myrmic).await;
    bridge_test(&myrmic).await;
}

#[step]
async fn runtime_management<B>(myrmic: &Myrmic<B>)
where
    B: MyrmicBackend + Clone,
{
    // start a runtime with a generated unique name
    let rt = myrmic.start_runtime_with_random_name(&[]).await;

    // start_runtime already waited for it to be listed
    let runtimes = myrmic.list_runtimes().await;
    assert!(runtimes.contains(&rt.name().to_owned()));
    let name = rt.name().to_owned();

    // delete our runtime; delete waits until it is gone
    rt.delete().await;
    let runtimes = myrmic.list_runtimes().await;
    assert!(!runtimes.contains(&name));

    // cleanup, stops docker containers that were started in case of docker backend
    myrmic.cleanup().await;
}

#[step]
async fn deploy_cell<B>(myrmic: &Myrmic<B>)
where
    B: MyrmicBackend + Clone,
{
    let cell_name = format!("Cell{}", uuid::Uuid::new_v4().as_simple());

    // start a runtime (waits until listed); dropped at the end -> guard deletes it
    let _rt = myrmic.start_runtime_with_random_name(&[]).await;

    // create and deploy a new cell with a generated SRI (deploy waits until deployed)
    let cell_spec = myrmic.new_cell(cell_name.as_str(), None).await;
    let cell = myrmic.deploy_with_random_sri(cell_spec, &[]).await;
    let sri = cell.sri().to_owned();
    assert!(myrmic.is_sri_deployed(&sri).await);

    // delete the cell (waits until gone from status)
    cell.delete().await;
    assert!(!myrmic.is_sri_deployed(&sri).await);
}

#[step]
async fn bridge_test(myrmic: &Myrmic<LocalBinary>) {
    // start HTTP mock server — bridge_http.yml points to localhost:10000
    let listener = std::net::TcpListener::bind("0.0.0.0:10000").unwrap();
    let mock_server = MockServer::builder().listener(listener).start().await;
    Mock::given(method("GET"))
        .and(path("/test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"hello": "world"})),
        )
        .mount(&mock_server)
        .await;

    // start MQTT broker — bridge_mqtt.yml connects to localhost:11000
    let mqtt = MqttBroker::start(11000).await;

    // subscribe to the MQTT egress output topic before deploying
    const MQTT_OUTPUT_TOPIC: &str = "e2e/test/output";
    let mut mqtt_sub = mqtt.subscribe(MQTT_OUTPUT_TOPIC).await;

    let _rt = myrmic.start_runtime_with_random_name(&[]).await;
    myrmic.deploy_app("assets/apps/app_spec.yml").await;

    // open a session into the swarm mesh; SorgHandle::connect waits until the
    // exec runtime is reachable, ensuring the DB service is also available.
    let session = myrmic.connect_session().await;
    let _sorg = SorgHandle::connect(session.clone()).await;

    // seed the DB template used by MQTT egress: ${db:e2e/test/data@topic}
    let db = DbHandle::new(&session);
    db.put_str(
        Scope::new("e2e", "test", "data"),
        "topic",
        MQTT_OUTPUT_TOPIC,
    )
    .await;

    // --- HTTP bridge test ---
    // send the test_http command; the cell calls the HTTP mock and returns the body
    let response = myrmic.send("bridge.test", "test_http").await;
    assert!(response.is_some(), "test_http returned no response");

    // --- MQTT bridge test ---
    // republish the ingress message until the bridge (which needs time to connect
    // and subscribe) delivers it to the cell and the egress message arrives
    let ingress_payload = serde_json::to_vec(&serde_json::json!("hello mqtt")).unwrap();
    let received = mqtt
        .publish_until_received(
            "e2e/test/ingress",
            ingress_payload.clone(),
            &mut mqtt_sub,
            std::time::Duration::from_secs(15),
        )
        .await;
    assert_eq!(
        received, ingress_payload,
        "MQTT round-trip payload mismatch"
    );
}
