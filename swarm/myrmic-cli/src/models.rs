use anyhow::Context as _;
use serde::Deserialize;
use sorg_common::{HttpBridgeConfig, MqttBridgeConfig, RestartPolicy, RestartType};
use std::collections::HashSet;
use std::time::Duration;

pub mod http;
pub mod mqtt;

#[cfg(test)]
mod tests;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    /// Application name. Stamped as the `app` on every cell in the bundle, so
    /// they group under it and `myrmic delete <name> --app` tears the bundle down.
    #[serde(default)]
    pub name: Option<String>,
    /// Buildable cell classes and bridge classes, keyed by `id`.
    #[serde(default)]
    pub classes: Vec<ClassDef>,
    /// Deployed instances, each referencing a `classes` entry by id.
    #[serde(default)]
    pub instances: Vec<Instance>,
}

/// A named entry under `classes:` — either a buildable cell or a bridge.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassDef {
    pub id: String,
    /// Build configuration for a cell class. A bare string is the crate path.
    #[serde(default)]
    pub build: Option<StringOr<Build>>,
    /// Path to a bridge specification file, relative to the app-specs file.
    #[serde(default, alias = "http", alias = "mqtt", alias = "cell")]
    pub spec: Option<std::path::PathBuf>,
}

/// The resolved kind of a [`ClassDef`].
pub enum ClassKind {
    /// A cell built from a crate.
    Cell(Build),
    /// A bridge described by a spec file.
    Bridge(std::path::PathBuf),
}

impl ClassDef {
    /// Resolve into `(id, kind)`.
    ///
    /// - `build` set        → [`ClassKind::Cell`] (bare string becomes the path)
    /// - a bridge spec set  → [`ClassKind::Bridge`]
    /// - neither            → [`ClassKind::Cell`] building the crate in the
    ///   app-specs folder (`path: "."`, automatic target)
    /// - both               → error
    pub fn resolve(self) -> anyhow::Result<(String, ClassKind)> {
        let ClassDef { id, build, spec } = self;
        match (build, spec) {
            (Some(_), Some(_)) => {
                anyhow::bail!("class `{id}` sets both a build and a bridge spec; use one")
            }
            (Some(StringOr::String(path)), None) => Ok((
                id,
                ClassKind::Cell(Build {
                    path,
                    ..Build::default()
                }),
            )),
            (Some(StringOr::Type(build)), None) => Ok((id, ClassKind::Cell(build))),
            (None, Some(spec)) => Ok((id, ClassKind::Bridge(spec))),
            (None, None) => Ok((id, ClassKind::Cell(Build::default()))),
        }
    }
}

/// A deployed instance under `instances:`, referencing a [`ClassDef`] by id.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    /// SRI to deploy under. Defaults to the referenced type id.
    #[serde(default, alias = "sri")]
    pub srn: Option<String>,
    /// References a cell class by id.
    #[serde(default)]
    pub class: Option<String>,
    /// References a bridge class by id.
    #[serde(default)]
    pub bridge: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Init arguments delivered to the cell's `#[init]` on deploy, encoded like
    /// a `send` payload (JSON by default; a value that isn't valid JSON is sent
    /// as a JSON string). Cell instances only; mutually exclusive with
    /// `init_file`.
    #[serde(default)]
    pub init: Option<String>,
    /// File whose raw bytes are delivered verbatim as the cell's `#[init]`
    /// arguments, relative to the app-specs file. Cell instances only; mutually
    /// exclusive with `init`.
    #[serde(default)]
    pub init_file: Option<std::path::PathBuf>,
    /// Restart policy for this instance. Only meaningful for app roots; the CLI
    /// merely carries it, host-side enforcement lives elsewhere.
    #[serde(default)]
    pub restart: Option<RestartSpec>,
}

/// The `restart:` field on an [`Instance`], in either the shorthand string form
/// (`always`) or the expanded map form that tunes the crash-loop bounds.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RestartSpec {
    /// `restart: on-error` — just the trigger, with default bounds.
    Shorthand(RestartTypeName),
    /// `restart: { type: on-error, max: 3, window: 30s, delay: 2s }`.
    Expanded(RestartExpanded),
}

/// The trigger names accepted for `restart` (in an app spec) and `--policy`
/// (on the command line), matching [`RestartType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RestartTypeName {
    Never,
    #[serde(alias = "onerror")]
    #[value(alias = "onerror")]
    OnError,
    Always,
}

impl RestartTypeName {
    /// The named trigger with [`RestartPolicy`]'s default crash-loop bounds.
    pub fn to_policy(self) -> RestartPolicy {
        RestartPolicy {
            restart_type: self.into(),
            ..RestartPolicy::default()
        }
    }
}

impl From<RestartTypeName> for RestartType {
    fn from(name: RestartTypeName) -> Self {
        match name {
            RestartTypeName::Never => RestartType::Never,
            RestartTypeName::OnError => RestartType::OnError,
            RestartTypeName::Always => RestartType::Always,
        }
    }
}

/// The expanded `restart` map. `type` is required; the bounds fall back to
/// [`RestartPolicy`]'s defaults (5 / 60s / 1s) when omitted. `window`/`delay`
/// accept human durations (`30s`, `500ms`, `1m`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestartExpanded {
    #[serde(rename = "type")]
    pub restart_type: RestartTypeName,
    #[serde(default)]
    pub max: Option<u32>,
    #[serde(default, deserialize_with = "de_human_duration")]
    pub window: Option<Duration>,
    #[serde(default, deserialize_with = "de_human_duration")]
    pub delay: Option<Duration>,
}

fn de_human_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        Some(s) => humantime::parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

impl RestartSpec {
    /// Resolve into a [`RestartPolicy`], applying default bounds for any field
    /// the author omitted.
    pub fn to_policy(&self) -> RestartPolicy {
        let defaults = RestartPolicy::default();
        match self {
            RestartSpec::Shorthand(name) => name.to_policy(),
            RestartSpec::Expanded(e) => RestartPolicy {
                restart_type: e.restart_type.into(),
                max_restarts: e.max.unwrap_or(defaults.max_restarts),
                window_ms: e.window.map_or(defaults.window_ms, |d| {
                    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
                }),
                delay_ms: e.delay.map_or(defaults.delay_ms, |d| {
                    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
                }),
            },
        }
    }
}

/// Which kind of [`ClassDef`] an [`Instance`] points at, and the id it names.
pub enum InstanceRef {
    Class(String),
    Bridge(String),
}

impl Instance {
    /// Resolve the instance's type reference, requiring exactly one of
    /// `class`/`bridge`.
    pub fn reference(&self) -> anyhow::Result<InstanceRef> {
        match (&self.class, &self.bridge) {
            (Some(_), Some(_)) => {
                anyhow::bail!("instance references both a class and a bridge; use one")
            }
            (Some(id), None) => Ok(InstanceRef::Class(id.clone())),
            (None, Some(id)) => Ok(InstanceRef::Bridge(id.clone())),
            (None, None) => anyhow::bail!("instance must reference a class or a bridge"),
        }
    }

    /// Whether the instance specifies any `#[init]` arguments.
    pub fn has_init(&self) -> bool {
        self.init.is_some() || self.init_file.is_some()
    }

    /// Resolve the mutually-exclusive `init`/`init_file` fields into the raw
    /// argument buffer delivered to the cell's `#[init]`. `init_file` is read
    /// relative to `folder` (the app-specs directory). The bytes are forwarded
    /// verbatim; the cell's `#[init]` decodes them.
    pub fn init_arguments(&self, folder: &std::path::Path) -> anyhow::Result<Option<Vec<u8>>> {
        match (&self.init, &self.init_file) {
            (Some(_), Some(_)) => {
                anyhow::bail!("instance sets both `init` and `init_file`; use one")
            }
            (Some(payload), None) => Ok(Some(crate::payload::encode(payload.clone(), false)?)),
            (None, Some(path)) => {
                let full = folder.join(path);
                let bytes = std::fs::read(&full)
                    .with_context(|| format!("failed to read init file '{}'", full.display()))?;
                Ok(Some(bytes))
            }
            (None, None) => Ok(None),
        }
    }
}

/// Internal representation of a resolved cell instance (built from an
/// [`Instance`] that references a cell type, or reconstructed from a `.nest`).
#[derive(Debug, Deserialize)]
pub struct CellInstance {
    pub id: String,
    #[serde(default, alias = "sri")]
    pub srn: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Resolved `#[init]` argument buffer for this instance, if any.
    #[serde(default)]
    pub arguments: Option<Vec<u8>>,
    /// Restart policy carried onto the deploy request. `None` when the author
    /// declared none, which deploys as [`RestartPolicy::default`] (`Never`) but
    /// lets `--policy` replace it without warning about a lost preference.
    #[serde(default)]
    pub restart: Option<RestartPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Crate directory (or `Cargo.toml`) to build, relative to the app-specs
    /// file. Defaults to `"."` — the app-specs folder itself.
    #[serde(
        default = "default_build_path",
        alias = "source",
        alias = "directory",
        alias = "dir"
    )]
    pub path: String,
    /// Which cargo target within the crate to build (`auto`, `lib`, or a
    /// target name). A class produces exactly one artifact, so at most one
    /// target may be named. Defaults to `auto`.
    #[serde(default)]
    pub target: Option<TargetSpec>,
    /// Which platforms to compile for (`linux`, `esp32c3`, `esp32c6`, `api`).
    /// Defaults to the CLI's default platform set.
    #[serde(default, alias = "platform")]
    pub platforms: Option<PlatformSpec>,
    #[serde(default)]
    pub features: HashSet<String>,
}

fn default_build_path() -> String {
    ".".to_string()
}

impl Default for Build {
    fn default() -> Self {
        Build {
            path: default_build_path(),
            target: None,
            platforms: None,
            features: HashSet::new(),
        }
    }
}

impl Build {
    /// Resolve `target` into a single [`CargoTarget`].
    ///
    /// A cell class produces exactly one artifact, so naming more than one
    /// target (via a list or a comma-separated string) is an error.
    pub fn cargo_target(&self) -> anyhow::Result<CargoTarget> {
        let selectors: Vec<String> = match &self.target {
            None => return Ok(CargoTarget::Auto),
            Some(StringOr::String(s)) => s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            Some(StringOr::Type(list)) => list
                .iter()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
        };

        match selectors.as_slice() {
            [] => Ok(CargoTarget::Auto),
            [one] => one.parse(),
            many => anyhow::bail!(
                "a class builds exactly one artifact, but {} targets were requested ({}); \
                 split them into separate classes",
                many.len(),
                many.join(", "),
            ),
        }
    }
}

/// A cargo target selector within a crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoTarget {
    /// Resolve automatically: the sole binary if there is exactly one,
    /// otherwise the sole library.
    Auto,
    /// The crate's library target (built as a `cdylib` wasm module).
    Lib,
    /// A named target, resolved against the crate's binaries (preferred) then
    /// its library.
    Named(String),
}

impl std::str::FromStr for CargoTarget {
    type Err = anyhow::Error;

    // `auto` is intentionally not parseable: it's the default you get by
    // omitting the selector, not a value you spell out.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        match s {
            "" => anyhow::bail!("empty cargo target; use `lib` or a target name"),
            "lib" => Ok(Self::Lib),
            "auto" => anyhow::bail!(
                "`auto` is the default target selection; omit `--target` rather than naming `auto`"
            ),
            _ => Ok(Self::Named(s.to_owned())),
        }
    }
}

pub type TargetSpec = StringOr<Vec<String>>;
pub type PlatformSpec = StringOr<Vec<String>>;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringOr<T> {
    String(String),
    Type(T),
}

macro_rules! try_from_yaml_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $variant:ident($ty:ty)
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(::serde::Deserialize)]
        $vis enum $name {
            $( $variant($ty) ),+
        }

        impl ::std::str::FromStr for $name {
            type Err = ::anyhow::Error;

            fn from_str(content: &str) -> ::std::result::Result<Self, Self::Err> {
                let mut found: ::std::option::Option<(Self, &'static str)> = None;
                let mut errs: ::std::vec::Vec<String> = ::std::vec::Vec::new();

                $(
                    let name = stringify!($ty);
                    match ::serde_yaml::from_str::<$ty>(content) {
                        Ok(v) => match found {
                            Some((_, other)) => panic!("[internal error] type overlap: {} vs {}", other, name),
                            None    => found = Some((Self::$variant(v), name)),
                        },
                        Err(e) => errs.push(e.to_string()),
                    }
                )+

                found.map(|(value, _name)| value).ok_or_else(|| ::anyhow::anyhow!("{}", errs.join(" OR ")))
            }
        }
    };
}

try_from_yaml_enum! {
    pub enum BridgeInput {
        Mqtt(mqtt::UserMqttBridge),
        Http(http::UserHttpBridgeApi),
    }
}

try_from_yaml_enum! {
    pub enum BuildInput {
        App(App),
    }
}

try_from_yaml_enum! {
    pub enum DeployInput {
        App(App),
        Mqtt(mqtt::UserMqttBridge),
        Http(http::UserHttpBridgeApi),
        MqttNest(MqttBridgeConfig),
        HttpEgressNest(HttpBridgeConfig),
    }
}

impl DeployInput {
    /// Whether this target deploys cells that carry a restart policy. Bridges
    /// do not — only a root wasm cell is restartable. Exhaustive on purpose: a
    /// new variant has to state its answer rather than defaulting to "no".
    pub fn carries_restart_policy(&self) -> bool {
        match self {
            DeployInput::App(_) => true,
            DeployInput::Mqtt(_)
            | DeployInput::Http(_)
            | DeployInput::MqttNest(_)
            | DeployInput::HttpEgressNest(_) => false,
        }
    }
}

/// Where a scaffolded crate gets `myrmic-sdk` from. Renders as the right-hand
/// side of a `[dependencies]` entry.
pub enum CargoDep {
    /// A published release, as a cargo version requirement.
    Version(String),
    Git(String, Option<String>),
    Path(std::path::PathBuf),
}

/// Anything cargo accepts in `crate = "…"`: a version or a comparator (`^0.2`,
/// `>=0.2, <0.3`, `*`). Checked before the path fallback, so a directory named
/// like a version needs a `./` prefix.
fn looks_like_version_req(value: &str) -> bool {
    value.starts_with(|ch: char| {
        ch.is_ascii_digit() || matches!(ch, '^' | '~' | '=' | '<' | '>' | '*')
    })
}

impl std::str::FromStr for CargoDep {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Probably a cleaner way to do it, but it works for now...
        let dep = if looks_like_version_req(value) {
            Self::Version(String::from(value))
        } else if value.starts_with("ssh://git") {
            let (url, rev) = if let Some((url, rev)) = value.split_once("?rev=") {
                (url, Some(rev))
            } else {
                (value, None)
            };

            let url = String::from(url);
            let rev = rev.map(String::from);
            Self::Git(url, rev)
        } else {
            let sdk = std::path::PathBuf::from(value);
            if !sdk.exists() {
                anyhow::bail!("unable to locate {} on the local fs", sdk.display());
            }

            Self::Path(sdk)
        };

        Ok(dep)
    }
}

impl std::fmt::Display for CargoDep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CargoDep::Version(req) => write!(f, r#""{req}""#),
            CargoDep::Path(path) => write!(f, r#"{{ path = "{}" }}"#, path.display()),
            CargoDep::Git(git, None) => write!(f, r#"{{ git = "{git}" }}"#),
            CargoDep::Git(git, Some(rev)) => {
                write!(f, r#"{{ git = "{git}", rev = "{rev}" }}"#)
            }
        }
    }
}
