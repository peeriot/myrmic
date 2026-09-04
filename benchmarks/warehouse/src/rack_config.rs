//! `[specialized.rack]` config shared between `warehouse-bench` (runs the benchmark against the
//! rack) and `rack-ctl` (uploads `myrmic` to the rack / tears it down) — see
//! `benchmarks/warehouse/run_rack.sh`, which drives both around one config file.

use serde::Deserialize;
use test_framework::rack::HostSpec;

/// One remote host's SSH destination, as written in the `[specialized.rack]` table.
#[derive(Deserialize)]
pub struct RackHost {
    /// e.g. `peeriot@rack-node-3.peeriot.intra`
    pub host: String,
}

/// `[specialized.rack]` — deploys the same topology across real hosts over SSH instead of
/// replicating cells inside one local swarm process. `zones`/`objects` must list exactly
/// `specialized.num_zones`/`specialized.num_objects` hosts, one cell replica per host; `central`
/// is the one host running the (non-replicated) central-tier cell.
#[derive(Deserialize)]
pub struct RackConfig {
    /// path to the myrmic binary on every host, already uploaded there (see
    /// `test_framework::rack::upload_binary`)
    #[serde(default = "default_rack_myrmic_path")]
    pub myrmic_path: String,
    /// zenoh endpoint the harness's own client session connects through to reach the rack mesh —
    /// typically an SSH-tunneled `tcp/127.0.0.1:<port>` pointed at `central` below.
    pub zenoh_connect: String,
    /// TCP port `central`'s runtime listens on for the above — myrmic's default listen port is
    /// ephemeral, so this pins it to something the tunnel/`zenoh_connect` can reliably target.
    #[serde(default = "default_rack_central_listen_port")]
    pub central_listen_port: u16,
    /// Pin a replica of every deployed cell's scope onto the host that runs it,
    /// before the cells load (see `crate::prealloc`).
    ///
    /// Off by default: it makes the benchmark declare a topology production
    /// could not know, and it is only one of the two ways to get writes off a
    /// single node — the other is a per-scope fallback pick, which needs no
    /// configuration at all. Leaving this a knob keeps both measurable.
    #[serde(default)]
    pub preallocate_replicas: bool,
    pub central: RackHost,
    pub zones: Vec<RackHost>,
    pub objects: Vec<RackHost>,
}

pub fn default_rack_myrmic_path() -> String {
    "myrmic".to_owned()
}

pub fn default_rack_central_listen_port() -> u16 {
    7447
}

pub const RACK_CENTRAL_TAG: &str = "central";

/// Where each rack host's runtime writes its telemetry files
/// (`test_framework::rack::RackTelemetry::Files`); wiped per provisioning.
pub const RACK_TELEMETRY_DIR: &str = "/tmp/swarm-telemetry";

pub fn rack_zone_tag(n: usize) -> String {
    format!("zone-{n}")
}

pub fn rack_object_tag(n: usize) -> String {
    format!("object-{n}")
}

/// Every host in `rack`'s deployment, each tagged with the unique tier/index tag its pinned cell
/// load will require (see `WarehouseScenario::build_ctx`).
///
/// # Panics
///
/// Panics if `rack.zones`/`rack.objects` don't list exactly `num_zones`/`num_objects` hosts.
#[must_use]
pub fn rack_host_specs(rack: &RackConfig, num_zones: usize, num_objects: usize) -> Vec<HostSpec> {
    assert_eq!(
        rack.zones.len(),
        num_zones,
        "specialized.rack.zones must list exactly num_zones ({num_zones}) hosts"
    );
    assert_eq!(
        rack.objects.len(),
        num_objects,
        "specialized.rack.objects must list exactly num_objects ({num_objects}) hosts"
    );

    let mut hosts = vec![
        HostSpec::new(
            rack.central.host.clone(),
            RACK_CENTRAL_TAG.to_owned(),
            vec![RACK_CENTRAL_TAG.to_owned()],
        )
        .with_listen_port(rack.central_listen_port),
    ];
    hosts.extend(rack.zones.iter().enumerate().map(|(n, h)| {
        let tag = rack_zone_tag(n);
        HostSpec::new(h.host.clone(), tag.clone(), vec![tag])
    }));
    hosts.extend(rack.objects.iter().enumerate().map(|(n, h)| {
        let tag = rack_object_tag(n);
        HostSpec::new(h.host.clone(), tag.clone(), vec![tag])
    }));
    hosts
}

/// The subset of a warehouse benchmark config `rack-ctl` needs: `num_objects`/`num_zones` (to
/// validate `rack`'s host lists) and `rack` itself. Deliberately not the full `Specialized` type
/// in `main.rs` — `rack-ctl` has no use for `fan_out_strategy`, `producer`, etc.
#[derive(Deserialize)]
pub struct RackCtlSpecialized {
    pub num_objects: usize,
    pub num_zones: usize,
    pub rack: Option<RackConfig>,
}

/// A benchmark TOML config file, as far as `rack-ctl` cares: just `[specialized]`'s rack-relevant
/// fields, ignoring `timeout`/`drain_timeout`/etc.
#[derive(Deserialize)]
pub struct RackCtlConfig {
    pub specialized: RackCtlSpecialized,
}

impl RackCtlConfig {
    /// Reads and parses a TOML config file at `path`.
    ///
    /// # Panics
    ///
    /// Panics if the file can't be read, doesn't parse, or has no `[specialized.rack]` table.
    #[must_use]
    pub fn load(path: &std::path::Path) -> Self {
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read config {}: {err}", path.display()));
        toml::from_str(&contents)
            .unwrap_or_else(|err| panic!("failed to parse config {}: {err}", path.display()))
    }

    /// This config's rack hosts, panicking if it has no `[specialized.rack]` table at all — both
    /// `rack-ctl` subcommands are meaningless without one.
    #[must_use]
    pub fn host_specs(&self) -> Vec<HostSpec> {
        let rack = self
            .specialized
            .rack
            .as_ref()
            .expect("config has no [specialized.rack] table");
        rack_host_specs(
            rack,
            self.specialized.num_zones,
            self.specialized.num_objects,
        )
    }

    #[must_use]
    pub fn myrmic_path(&self) -> &str {
        &self
            .specialized
            .rack
            .as_ref()
            .expect("config has no [specialized.rack] table")
            .myrmic_path
    }
}
