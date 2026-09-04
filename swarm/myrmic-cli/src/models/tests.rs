use super::*;
use std::str::FromStr;

fn build(yaml: &str) -> Build {
    serde_yaml::from_str(yaml).expect("valid Build yaml")
}

fn class_def(yaml: &str) -> ClassDef {
    serde_yaml::from_str(yaml).expect("valid ClassDef yaml")
}

fn instance(yaml: &str) -> Instance {
    serde_yaml::from_str(yaml).expect("valid Instance yaml")
}

/// Resolve a cell-class yaml into its [`Build`], panicking if it's a bridge.
fn cell_build(yaml: &str) -> Build {
    match class_def(yaml).resolve().expect("resolves") {
        (_, ClassKind::Cell(build)) => build,
        (_, ClassKind::Bridge(_)) => panic!("expected a cell class"),
    }
}

#[test]
fn cargo_target_parses_lib_and_named() {
    assert_eq!(CargoTarget::from_str("lib").unwrap(), CargoTarget::Lib);
    assert_eq!(
        CargoTarget::from_str("server").unwrap(),
        CargoTarget::Named("server".into())
    );
    assert_eq!(
        CargoTarget::from_str("  server  ").unwrap(),
        CargoTarget::Named("server".into())
    );
}

#[test]
fn cargo_target_rejects_the_auto_literal() {
    // `auto` is the default (no `--target` / no `target:`), never a spelled value.
    assert!(CargoTarget::from_str("auto").is_err());
}

#[test]
fn cargo_target_rejects_empty() {
    assert!(CargoTarget::from_str("").is_err());
    assert!(CargoTarget::from_str("   ").is_err());
}

#[test]
fn build_path_defaults_to_dot() {
    assert_eq!(build("{}").path, ".");
    assert_eq!(build("target: server").path, ".");
}

#[test]
fn build_cargo_target_defaults_to_auto() {
    assert_eq!(build("{}").cargo_target().unwrap(), CargoTarget::Auto);
}

#[test]
fn build_parses_single_named_target() {
    assert_eq!(
        build("target: server").cargo_target().unwrap(),
        CargoTarget::Named("server".into())
    );
    // single-element list form is also one target
    assert_eq!(
        build("target: [server]").cargo_target().unwrap(),
        CargoTarget::Named("server".into())
    );
}

#[test]
fn build_rejects_multiple_cargo_targets_list_form() {
    let err = build("target: [a, b]")
        .cargo_target()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("one artifact") || err.contains("exactly one"),
        "error should explain one-target-per-class: {err}"
    );
}

#[test]
fn build_rejects_multiple_cargo_targets_comma_string() {
    assert!(build("target: \"a, b\"").cargo_target().is_err());
}

#[test]
fn build_path_still_accepts_aliases() {
    assert_eq!(build("source: ./foo").path, "./foo");
    assert_eq!(build("dir: ./bar").path, "./bar");
}

#[test]
fn class_build_omitted_defaults_to_dot_auto() {
    let resolved = cell_build("id: server");
    assert_eq!(resolved.path, ".");
    assert_eq!(resolved.cargo_target().unwrap(), CargoTarget::Auto);
}

#[test]
fn class_build_string_form_is_path() {
    let resolved = cell_build("{ id: fleet, build: ./cells/fleet }");
    assert_eq!(resolved.path, "./cells/fleet");
    assert_eq!(resolved.cargo_target().unwrap(), CargoTarget::Auto);
}

#[test]
fn class_build_map_form_carries_target() {
    let resolved = cell_build("{ id: server, build: { target: server } }");
    assert_eq!(resolved.path, ".");
    assert_eq!(
        resolved.cargo_target().unwrap(),
        CargoTarget::Named("server".into())
    );
}

#[test]
fn class_spec_and_its_aliases_resolve_to_a_bridge() {
    for key in ["spec", "http", "mqtt", "cell"] {
        let yaml = format!("{{ id: st, {key}: ./bridges/st.yml }}");
        match class_def(&yaml).resolve().expect("resolves") {
            (id, ClassKind::Bridge(path)) => {
                assert_eq!(id, "st");
                assert_eq!(path, std::path::PathBuf::from("./bridges/st.yml"));
            }
            (_, ClassKind::Cell(_)) => panic!("expected a bridge for key `{key}`"),
        }
    }
}

#[test]
fn class_rejects_build_and_spec_together() {
    assert!(
        class_def("{ id: x, build: ., http: ./b.yml }")
            .resolve()
            .is_err()
    );
}

#[test]
fn instance_resolves_class_and_bridge_references() {
    assert!(matches!(
        instance("{ class: blah }").reference().unwrap(),
        InstanceRef::Class(id) if id == "blah"
    ));
    assert!(matches!(
        instance("{ sri: st, bridge: spacetraders }").reference().unwrap(),
        InstanceRef::Bridge(id) if id == "spacetraders"
    ));
}

#[test]
fn instance_rejects_missing_or_ambiguous_reference() {
    assert!(instance("{ sri: agent }").reference().is_err());
    assert!(instance("{ class: a, bridge: b }").reference().is_err());
}

#[test]
fn instance_has_init_reflects_init_fields() {
    assert!(!instance("{ class: a }").has_init());
    assert!(instance("{ class: a, init: hello }").has_init());
    assert!(instance("{ class: a, init_file: ./seed.bin }").has_init());
}

#[test]
fn instance_init_arguments_defaults_to_none() {
    let args = instance("{ class: a }")
        .init_arguments(std::path::Path::new("."))
        .unwrap();
    assert!(args.is_none());
}

#[test]
fn instance_init_arguments_encodes_like_a_send_payload() {
    // Valid JSON is passed through; a bare value is wrapped as a JSON string.
    let json = instance("{ class: a, init: '42' }")
        .init_arguments(std::path::Path::new("."))
        .unwrap();
    assert_eq!(json.as_deref(), Some(b"42".as_slice()));

    let string = instance("{ class: a, init: hello }")
        .init_arguments(std::path::Path::new("."))
        .unwrap();
    assert_eq!(string.as_deref(), Some(b"\"hello\"".as_slice()));
}

#[test]
fn restart_defaults_to_none() {
    assert!(instance("{ class: a }").restart.is_none());
}

/// The spellings `--policy` accepts, including the run-together `onerror`.
#[test]
fn restart_type_name_parses_policy_spellings() {
    use clap::ValueEnum as _;

    let parse = |s: &str| RestartTypeName::from_str(s, true).expect("known policy");
    assert_eq!(parse("never"), RestartTypeName::Never);
    assert_eq!(parse("on-error"), RestartTypeName::OnError);
    assert_eq!(parse("onerror"), RestartTypeName::OnError);
    assert_eq!(parse("always"), RestartTypeName::Always);

    assert!(RestartTypeName::from_str("sometimes", true).is_err());
}

/// `--policy` sets the trigger only; the crash-loop bounds stay at defaults.
#[test]
fn restart_type_name_to_policy_keeps_default_bounds() {
    let defaults = sorg_common::RestartPolicy::default();
    assert_eq!(
        RestartTypeName::Always.to_policy(),
        sorg_common::RestartPolicy {
            restart_type: sorg_common::RestartType::Always,
            ..defaults
        }
    );
}

/// The `restart:` shorthand accepts the same `onerror` spelling as `--policy`.
#[test]
fn restart_shorthand_accepts_onerror_spelling() {
    let policy = instance("{ class: a, restart: onerror }")
        .restart
        .expect("restart set")
        .to_policy();
    assert_eq!(policy.restart_type, sorg_common::RestartType::OnError);
}

#[test]
fn restart_shorthand_maps_to_policy() {
    let policy = instance("{ class: a, restart: always }")
        .restart
        .expect("restart set")
        .to_policy();
    // Shorthand keeps the default crash-loop bounds.
    assert_eq!(
        policy,
        sorg_common::RestartPolicy {
            restart_type: sorg_common::RestartType::Always,
            ..Default::default()
        }
    );

    let never = instance("{ class: a, restart: never }")
        .restart
        .expect("restart set")
        .to_policy();
    assert_eq!(never.restart_type, sorg_common::RestartType::Never);
}

#[test]
fn restart_expanded_parses_bounds_and_durations() {
    let policy =
        instance("{ class: a, restart: { type: on-error, max: 3, window: 30s, delay: 2s } }")
            .restart
            .expect("restart set")
            .to_policy();
    assert_eq!(
        policy,
        sorg_common::RestartPolicy {
            restart_type: sorg_common::RestartType::OnError,
            max_restarts: 3,
            window_ms: 30_000,
            delay_ms: 2_000,
        }
    );
}

#[test]
fn restart_expanded_applies_defaults_for_omitted_fields() {
    let policy = instance("{ class: a, restart: { type: on-error } }")
        .restart
        .expect("restart set")
        .to_policy();
    let defaults = sorg_common::RestartPolicy::default();
    assert_eq!(policy.restart_type, sorg_common::RestartType::OnError);
    assert_eq!(policy.max_restarts, defaults.max_restarts);
    assert_eq!(policy.window_ms, defaults.window_ms);
    assert_eq!(policy.delay_ms, defaults.delay_ms);
}

#[test]
fn instance_init_and_init_file_are_mutually_exclusive() {
    let err = instance("{ class: a, init: hello, init_file: ./seed.bin }")
        .init_arguments(std::path::Path::new("."))
        .unwrap_err()
        .to_string();
    assert!(err.contains("init") && err.contains("init_file"), "{err}");
}

#[test]
fn class_rejects_unknown_field() {
    // Regression: a build option written on the class instead of inside `build:`
    // (here `platform:`) used to be silently dropped, so the cell built
    // linux-only and only failed much later as a placement "missing artifact"
    // error. It must be rejected at parse time instead.
    let err = serde_yaml::from_str::<ClassDef>("{ id: sensor, build: sensor, platform: esp32c6 }")
        .expect_err("a stray class field must be rejected")
        .to_string();
    assert!(
        err.contains("platform"),
        "error should name the stray field: {err}"
    );
}

#[test]
fn class_map_form_carries_platform() {
    // The supported spelling keeps build options inside the `build:` map.
    let resolved = cell_build("{ id: sensor, build: { path: sensor, platform: esp32c6 } }");
    assert_eq!(resolved.path, "sensor");
    assert!(
        resolved.platforms.is_some(),
        "platform inside build: must be parsed"
    );
}

#[test]
fn build_rejects_unknown_field() {
    // A typo inside `build:` (here `platfrom`) must error rather than silently
    // fall back to the default platform set.
    assert!(serde_yaml::from_str::<Build>("{ path: sensor, platfrom: esp32c6 }").is_err());
}

#[test]
fn instance_rejects_unknown_field() {
    // A typo on an instance (here `tag` for `tags`) must error.
    assert!(serde_yaml::from_str::<Instance>("{ class: a, tag: foo }").is_err());
}

#[test]
fn cargo_dep_version_renders_as_a_registry_dep() {
    let dep = CargoDep::from_str("0.2.1").expect("a bare version is a registry dep");
    assert_eq!(dep.to_string(), r#""0.2.1""#);
}

#[test]
fn cargo_dep_version_requirements_parse_as_versions() {
    for req in ["^0.2", "~0.2.1", "=0.2.1", ">=0.2, <0.3", "*"] {
        let dep = CargoDep::from_str(req).unwrap_or_else(|err| panic!("{req}: {err}"));
        assert_eq!(dep.to_string(), format!(r#""{req}""#));
    }
}

#[test]
fn cargo_dep_git_renders_an_inline_table() {
    let dep = CargoDep::from_str("ssh://git@github.com/peeriot/swarm.git?rev=abc12345").unwrap();
    assert_eq!(
        dep.to_string(),
        r#"{ git = "ssh://git@github.com/peeriot/swarm.git", rev = "abc12345" }"#
    );

    let dep = CargoDep::from_str("ssh://git@github.com/peeriot/swarm.git").unwrap();
    assert_eq!(
        dep.to_string(),
        r#"{ git = "ssh://git@github.com/peeriot/swarm.git" }"#
    );
}

#[test]
fn cargo_dep_path_renders_an_inline_table() {
    let here = env!("CARGO_MANIFEST_DIR");
    let dep = CargoDep::from_str(here).expect("the crate dir exists");
    assert_eq!(dep.to_string(), format!(r#"{{ path = "{here}" }}"#));
}

#[test]
fn cargo_dep_rejects_a_missing_path() {
    assert!(CargoDep::from_str("does/not/exist").is_err());
}
