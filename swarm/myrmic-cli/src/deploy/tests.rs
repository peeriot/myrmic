use std::collections::{BTreeMap, HashMap};

use cell_protocol::Sri;
use sorg_common::{
    BodyTemplate, CellConfig, CellDeployment, DeployRequest, HttpBridgeApi, MqttBridge,
    RequirementTags, RestartPolicy, RestartType, WireHttpEndpoint, WireHttpRequestTemplate,
    WireHttpResponseVariant, WireMqttEgress, WireMqttIngress,
};

use crate::args::Ctx;
use crate::build::{AppInfo, CellClass};
use crate::models::{CellInstance, RestartTypeName};

use super::build_deploy_request;

// Validates that the CLI's AppInfo-to-DeployRequest conversion produces
// the same request structure used by the integration test in
// sorg-orchestration/tests/integration/app_deployment/mod.rs
// (see `build_app_deployment_request` and `deploy_and_exercise`).
#[test]
#[allow(clippy::too_many_lines)]
fn build_deploy_request_matches_integration_test() {
    let mock_server_url = "http://localhost:1234";

    let info = AppInfo {
        name: "test_app".to_owned(),
        instances: vec![
            CellInstance {
                id: "cell_1".to_owned(),
                srn: Some("cell_1".to_owned()),
                tags: vec![],
                arguments: None,
                restart: None,
            },
            CellInstance {
                id: "cell_2".to_owned(),
                srn: Some("cell_2".to_owned()),
                tags: vec![],
                arguments: None,
                restart: None,
            },
        ],
        classes: [
            (
                "cell_1".to_owned(),
                CellClass {
                    name: "cell_1.wasm".to_owned(),
                    wasm_path: None,
                    riscv32imac: None,
                },
            ),
            (
                "cell_2".to_owned(),
                CellClass {
                    name: "cell_2.wasm".to_owned(),
                    wasm_path: None,
                    riscv32imac: None,
                },
            ),
        ]
        .into_iter()
        .collect(),
        mqtt_bridges: vec![MqttBridge {
            cell_name: "mqtt_bridge".to_owned(),
            broker: "mqtt://localhost:1883".to_owned(),
            ingress: vec![WireMqttIngress {
                id: "ingest_trigger".to_owned(),
                topic: "test/ingress".to_owned(),
                qos: None,
                payload: BodyTemplate::String("payload".to_owned()),
            }],
            egress: vec![WireMqttEgress {
                id: "publish_result".to_owned(),
                topic: "test/egress".parse().unwrap(),
                qos: None,
                payload: BodyTemplate::String("payload".to_owned()),
            }],
        }],
        http_bridges: vec![HttpBridgeApi {
            cell_name: "http_bridge".to_owned(),
            base_url: mock_server_url.to_owned(),
            endpoints: vec![WireHttpEndpoint {
                id: "fetch_data".to_owned(),
                request: WireHttpRequestTemplate {
                    method: "POST".to_owned(),
                    path: "/api/data".parse().unwrap(),
                    query: BTreeMap::new(),
                    headers: BTreeMap::new(),
                    body: Some(BodyTemplate::String("data".to_owned())),
                    timeout_ms: None,
                },
                response: BTreeMap::from([(
                    200,
                    WireHttpResponseVariant {
                        headers: BTreeMap::new(),
                        body: Some(BodyTemplate::String("result".to_owned())),
                    },
                )]),
            }],
        }],
    };

    let expected = DeployRequest::new(vec![
        CellDeployment::new(
            Sri::from_uuid(cell_protocol::sri_of_path("cell_1").unwrap()),
            CellConfig::Wasm {
                class: "cell_1.wasm".to_owned(),
            },
        )
        .with_app(Some("test_app".to_owned())),
        CellDeployment::new(
            Sri::from_uuid(cell_protocol::sri_of_path("cell_2").unwrap()),
            CellConfig::Wasm {
                class: "cell_2.wasm".to_owned(),
            },
        )
        .with_app(Some("test_app".to_owned())),
        CellDeployment::new(
            Sri::from_uuid(cell_protocol::sri_of_path("mqtt_bridge").unwrap()),
            CellConfig::MqttBridge(MqttBridge {
                cell_name: "mqtt_bridge".to_owned(),
                broker: "mqtt://localhost:1883".to_owned(),
                ingress: vec![WireMqttIngress {
                    id: "ingest_trigger".to_owned(),
                    topic: "test/ingress".to_owned(),
                    qos: None,
                    payload: BodyTemplate::String("payload".to_owned()),
                }],
                egress: vec![WireMqttEgress {
                    id: "publish_result".to_owned(),
                    topic: "test/egress".parse().unwrap(),
                    qos: None,
                    payload: BodyTemplate::String("payload".to_owned()),
                }],
            }),
        )
        .with_tags(RequirementTags::new(vec!["linux"]))
        .with_app(Some("test_app".to_owned())),
        CellDeployment::new(
            Sri::from_uuid(cell_protocol::sri_of_path("http_bridge").unwrap()),
            CellConfig::HttpBridge(HttpBridgeApi {
                cell_name: "http_bridge".to_owned(),
                base_url: mock_server_url.to_owned(),
                endpoints: vec![WireHttpEndpoint {
                    id: "fetch_data".to_owned(),
                    request: WireHttpRequestTemplate {
                        method: "POST".to_owned(),
                        path: "/api/data".parse().unwrap(),
                        query: BTreeMap::new(),
                        headers: BTreeMap::new(),
                        body: Some(BodyTemplate::String("data".to_owned())),
                        timeout_ms: None,
                    },
                    response: BTreeMap::from([(
                        200,
                        WireHttpResponseVariant {
                            headers: BTreeMap::new(),
                            body: Some(BodyTemplate::String("result".to_owned())),
                        },
                    )]),
                }],
            }),
        )
        .with_tags(RequirementTags::new(vec!["linux"]))
        .with_app(Some("test_app".to_owned())),
    ]);

    let result = build_deploy_request(&info).expect("build deploy request");
    assert_eq!(result, expected);
}

/// `--policy` replaces whatever the app spec declared, including on instances
/// that declared nothing (which would otherwise deploy as `Never`).
#[test]
fn override_restart_replaces_every_instance_policy() {
    let mut info = AppInfo {
        name: "app".to_owned(),
        instances: vec![
            CellInstance {
                id: "declared".to_owned(),
                srn: Some("declared".to_owned()),
                tags: vec![],
                arguments: None,
                restart: Some(RestartPolicy {
                    restart_type: RestartType::Never,
                    max_restarts: 9,
                    ..RestartPolicy::default()
                }),
            },
            CellInstance {
                id: "silent".to_owned(),
                srn: Some("silent".to_owned()),
                tags: vec![],
                arguments: None,
                restart: None,
            },
        ],
        classes: HashMap::new(),
        mqtt_bridges: vec![],
        http_bridges: vec![],
    };

    let policy = RestartTypeName::Always.to_policy();
    super::override_restart(Ctx::default(), &mut info, &policy);

    for instance in &info.instances {
        assert_eq!(instance.restart.as_ref(), Some(&policy));
    }
}

/// An instance that declared no `restart:` still deploys as `Never`.
#[test]
fn undeclared_restart_deploys_as_never() {
    let info = AppInfo {
        name: "app".to_owned(),
        instances: vec![CellInstance {
            id: "cell".to_owned(),
            srn: Some("cell".to_owned()),
            tags: vec![],
            arguments: None,
            restart: None,
        }],
        classes: HashMap::from([(
            "cell".to_owned(),
            CellClass {
                name: "cell.wasm".to_owned(),
                wasm_path: None,
                riscv32imac: None,
            },
        )]),
        mqtt_bridges: vec![],
        http_bridges: vec![],
    };

    let request = build_deploy_request(&info).expect("build deploy request");
    assert_eq!(request.cells[0].restart, RestartPolicy::default());
}

/// The override message spells out the bounds, so a same-trigger override does
/// not read as `always -> always`.
#[test]
fn describe_restart_spells_out_trigger_and_bounds() {
    assert_eq!(
        super::describe_restart(&RestartTypeName::OnError.to_policy()),
        "on-error (max 5, window 60000ms, delay 1000ms)"
    );
}
