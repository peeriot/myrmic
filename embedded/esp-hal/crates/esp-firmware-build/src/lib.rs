//! Build-script support for `esp-firmware`.
//!
//! Call [`configure`] from your firmware crate's `build.rs`:
//!
//! ```no_run
//! esp_firmware_build::configure();
//! ```
//!
//! It turns an optional `partitions.toml` beside your `Cargo.toml` into the
//! AOT flash layout the firmware consumes and the app partition table
//! `espflash` consumes, and installs link-time bounds on the main stack and
//! the firmware image.
//!
//! Two knobs are exposed — the firmware (app) partition size and the AOT XIP
//! storage size. From those (plus the target chip and its flash size) a fully
//! contiguous physical layout is derived, validated, and emitted into
//! `$OUT_DIR`:
//!
//! * `esp_firmware_partition_layout.rs` — the layout expression
//!   [`partition_layout!`](../esp_firmware/macro.partition_layout.html) includes.
//! * `partitions.generated.csv` — an app-only ESP-IDF partition table
//!   (`nvs` / `phy_init` / `factory`).
//!
//! The AOT region is deliberately left as an unpartitioned gap: it is mapped
//! manually by `esp-mmu` and mutated at runtime, so the bootloader must neither
//! map nor validate it. Sizing `factory` to end exactly at the AOT boundary
//! makes `espflash` refuse to flash a firmware image that would overflow into
//! the AOT region — enforcing the ceiling with the stock bootloader.
//!
//! With no `partitions.toml` present, the layout `myrmic build` hands over in
//! [`PARTITIONS_ENV`] is used; without that either, a 4 MB layout.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// MMU / flash mapping granularity. All regions are aligned to this.
const PAGE: u64 = 0x1_0000; // 64 KiB
/// Metadata region size (always one page).
const META_LEN: u64 = PAGE;
/// Start of the `factory` (app) partition — right after bootloader, partition
/// table, `nvs` and `phy_init`.
const FACTORY_START: u64 = 0x1_0000;
/// Recommended minimum `aot_size`. Below this a usable WASM module is unlikely
/// to fit, so a cargo warning is emitted (but the build still proceeds).
const RECOMMENDED_MIN_AOT_SIZE: u64 = 200 * 1024;

/// Flash reserved, on top of the XIP extent the firmware-size guard measures,
/// for the parts of the esp-image the linker cannot see: the RAM-loaded
/// segments (`.data`/`.rwtext`, stored in the image and copied to RAM at boot)
/// and the esp-image / per-segment headers. Sized to cover a wifi+ble build's
/// RAM-stored segments with slack, so the guard stays a conservative link-time
/// bound (swarm#1355).
const FIRMWARE_IMAGE_MARGIN: u64 = 0x3_0000; // 192 KiB

/// Minimum main-stack size, enforced at link time. esp-hal sizes `.stack` as
/// whatever RWDATA is left after `.data`/`.bss`, so unrelated static growth
/// shrinks it silently (the `ble` feature costs ~39 `KiB` of `.bss` and leaves
/// ~8 `KiB` of stack).
///
/// The deep zenoh/db poll chains run on their own fixed-size thread stack
/// (`Config::net_stack`), so the main stack only carries the shallow tasks
/// (~6 `KiB` peak via `stack-hwm`). A build whose leftover drops below this
/// fails loudly instead of overflowing into `.bss`.
const MIN_MAIN_STACK: u64 = 0x1F00;

/// Default total flash assumed when `partitions.toml` names none.
const DEFAULT_FLASH_SIZE: u64 = 0x40_0000;
/// Default app partition size (see #1347).
const DEFAULT_FIRMWARE_SIZE: u64 = 0x30_0000;
/// Default AOT XIP storage size — the rest of the default flash.
const DEFAULT_AOT_SIZE: u64 = 0x10_0000;
/// Largest app partition a `ble` build is known not to fit: the NimBLE host
/// stack costs ~0.5 MB of flash and the image comes out around 2.06 MB.
const BLE_CRAMPED_FIRMWARE_SIZE: u64 = 0x20_0000;

/// Environment variable through which `myrmic build` hands a firmware crate its
/// partition layout when the crate has no `partitions.toml`. The value is the
/// compact form of [`Partitions::to_compact`]. A private contract between the
/// CLI and this crate, not a user-facing knob.
pub const PARTITIONS_ENV: &str = "ESP_FIRMWARE_PARTITIONS";

/// Per-chip constraints.
struct Chip {
    /// Feature name, used only for diagnostics.
    name: &'static str,
    /// Highest MMU page count.
    max_page_number: u64,
}

/// Raw config file schema. Every field is optional; a missing file or key falls
/// back to defaults.
#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    partitions: Partitions,
}

/// The partition knobs of `partitions.toml`. Every knob is optional; an unset
/// one takes its default when the layout is derived.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Partitions {
    /// Firmware partition size, bootloader included (e.g. `"2M"`, `"0x1F0000"`,
    /// `2031616`).
    #[serde(deserialize_with = "de_size")]
    pub firmware_size: Option<u64>,
    /// AOT XIP storage size. It includes both a 64 KB metadata region and the
    /// AOT XIP module.
    #[serde(deserialize_with = "de_size")]
    pub aot_size: Option<u64>,
    /// Total usable flash on the target device. Defaults to 4M.
    #[serde(deserialize_with = "de_size")]
    pub flash_size: Option<u64>,
}

impl Partitions {
    /// The layout `myrmic build` supplies to a crate without a `partitions.toml`:
    /// the AOT region keeps its default size and the firmware takes the rest of
    /// the flash. With no known flash size this is the 4 MB default.
    pub fn default_layout(flash_size: Option<u64>) -> Self {
        let flash_size = flash_size.unwrap_or(DEFAULT_FLASH_SIZE);
        Self {
            firmware_size: Some(flash_size.saturating_sub(DEFAULT_AOT_SIZE)),
            aot_size: Some(DEFAULT_AOT_SIZE),
            flash_size: Some(flash_size),
        }
    }

    /// The set knobs as comma-separated `key=0xHEX` pairs — the value carried in
    /// [`PARTITIONS_ENV`].
    pub fn to_compact(&self) -> String {
        [
            ("firmware", self.firmware_size),
            ("aot", self.aot_size),
            ("flash", self.flash_size),
        ]
        .into_iter()
        .filter_map(|(key, size)| size.map(|size| format!("{key}={size:#x}")))
        .collect::<Vec<_>>()
        .join(",")
    }

    /// Decodes the form produced by [`Self::to_compact`]. Sizes accept whatever
    /// `partitions.toml` accepts.
    pub fn parse_compact(compact: &str) -> Result<Self, String> {
        let mut partitions = Self::default();
        for pair in compact.split(',').filter(|pair| !pair.trim().is_empty()) {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("expected key=size, got {pair:?}"))?;
            let size = parse_size(value)
                .ok_or_else(|| format!("invalid size {value:?} for {}", key.trim()))?;
            let slot = match key.trim() {
                "firmware" => &mut partitions.firmware_size,
                "aot" => &mut partitions.aot_size,
                "flash" => &mut partitions.flash_size,
                other => return Err(format!("unknown partition knob {other:?}")),
            };
            *slot = Some(size);
        }
        Ok(partitions)
    }

    /// Chooses the knobs a build uses: a `partitions.toml` wins outright, then
    /// the layout `myrmic build` passed in [`PARTITIONS_ENV`], else defaults.
    ///
    /// # Panics
    ///
    /// Panics on a malformed env value — tooling writes it, so that is a bug.
    pub fn select(file: Option<Self>, env: Option<&str>) -> Self {
        if let Some(file) = file {
            return file;
        }
        match env {
            Some(compact) => Self::parse_compact(compact)
                .unwrap_or_else(|e| panic!("malformed {PARTITIONS_ENV}: {e}")),
            None => Self::default(),
        }
    }
}

/// The set knobs, spelled as `partitions.toml` lines so they can be pasted
/// into one.
impl std::fmt::Display for Partitions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let knobs = [
            ("firmware_size", self.firmware_size),
            ("aot_size", self.aot_size),
            ("flash_size", self.flash_size),
        ];
        let mut first = true;
        for (key, size) in knobs {
            let Some(size) = size else { continue };
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            write!(f, "{key} = \"{}\"", spell_size(size))?;
        }
        Ok(())
    }
}

/// The `[partitions]` knobs of the `partitions.toml` beside a crate's
/// `Cargo.toml`, or `None` when the crate ships no such file.
pub fn read_partitions_toml(manifest_dir: &Path) -> Result<Option<Partitions>, String> {
    read_partitions_toml_at(&manifest_dir.join("partitions.toml"))
}

/// The `[partitions]` knobs of a `partitions.toml`-format file at an explicit
/// path, or `None` when the file is absent. Used by a firmware crate whose build
/// script selects a partition file by some rule of its own (e.g. per chip).
pub fn read_partitions_toml_at(path: &Path) -> Result<Option<Partitions>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };
    let config: Config =
        toml::from_str(&text).map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

    Ok(Some(config.partitions))
}

/// `2M` / `1984K` for round sizes, hex otherwise — the forms `partitions.toml`
/// accepts.
pub fn spell_size(bytes: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = 1024 * K;
    if bytes > 0 && bytes.is_multiple_of(M) {
        format!("{}M", bytes / M)
    } else if bytes > 0 && bytes.is_multiple_of(K) {
        format!("{}K", bytes / K)
    } else {
        format!("{bytes:#X}")
    }
}

/// Generates the flash layout, the app partition table, and the main-stack and
/// firmware-size link asserts for the calling firmware crate.
///
/// # Panics
///
/// Panics if no `esp32c*` chip feature is enabled, if more than one is, if
/// `partitions.toml` is unreadable or malformed, or if the requested sizes do
/// not fit the chip's flash or MMU range.
pub fn configure() {
    let manifest_dir = manifest_dir();
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("partitions.toml").display()
    );
    let file = read_partitions_toml(&manifest_dir).unwrap_or_else(|e| panic!("{e}"));

    println!("cargo:rerun-if-env-changed={PARTITIONS_ENV}");
    let env = std::env::var(PARTITIONS_ENV).ok();

    configure_with_partitions(&Partitions::select(file, env.as_deref()));
}

/// Like [`configure`], but with the partition knobs already chosen — for a
/// firmware crate whose build script selects its partition file itself (for
/// example, one file per chip, read with [`read_partitions_toml_at`]). Any knob
/// left unset falls back to the 4 MB default.
///
/// # Panics
///
/// Panics for the same reasons as [`configure`], less the file/env parsing.
pub fn configure_with_partitions(partitions: &Partitions) {
    let chip = detect_chip();

    // Defaults to a 4 MB layout (3M `firmware_size` + 1M `aot_size`, see #1347).
    let firmware_size = partitions.firmware_size.unwrap_or(DEFAULT_FIRMWARE_SIZE);
    let aot_size = partitions.aot_size.unwrap_or(DEFAULT_AOT_SIZE);
    let flash_size = partitions.flash_size.unwrap_or(DEFAULT_FLASH_SIZE);

    if aot_size < RECOMMENDED_MIN_AOT_SIZE {
        println!(
            "cargo:warning=aot_size ({aot_size:#X}) is below the recommended minimum of \
             {RECOMMENDED_MIN_AOT_SIZE:#X} ({} KiB); a usable WASM module is unlikely to fit",
            RECOMMENDED_MIN_AOT_SIZE / 1024,
        );
    }

    warn_if_ble_is_cramped(&chip, firmware_size);

    let layout = Layout::derive(&chip, firmware_size, aot_size, flash_size);

    write_layout_rs(&layout);
    let csv = write_partitions_csv(&layout, firmware_size);
    write_espflash_config(&csv);
    write_stack_assert();
    write_firmware_size_assert(firmware_size);
}

/// Marks the files this build script owns, so a hand-written one is never
/// clobbered.
const GENERATED_MARKER: &str = "@generated by esp-firmware-build";

/// Points `espflash` at the generated partition table, so flashing enforces the
/// firmware ceiling without anyone having to remember `--partition-table`.
///
/// Without this, `cargo espflash flash` (which does not go through the cargo
/// `runner`) falls back to espflash's built-in table. That table is sized to
/// the whole chip, so an image which overflows *this* layout's app partition is
/// written anyway, straddles the AOT region, and the bootloader then rejects it
/// mid-image — a boot loop reporting `invalid segment length 0xffffffff`, with
/// nothing pointing at the partition table as the cause.
///
/// `espflash.toml` is read from the working directory or its parent, so this
/// covers `cargo espflash` run from the firmware crate or from a workspace
/// directly above it.
/// Entry point for build-time Signal Layer pipeline generation. See [`PipelineBuild`].
pub fn pipeline() -> PipelineBuild {
    PipelineBuild::default()
}

/// Builder for the Signal Layer codegen step. Point it at a board manifest and a
/// pipeline YAML, then call [`generate`](PipelineBuild::generate); the generated
/// Rust lands in `$OUT_DIR/pipeline.rs`, where `esp_firmware::pipeline!` includes it.
#[derive(Default)]
pub struct PipelineBuild {
    board: Option<PathBuf>,
    pipeline: Option<PathBuf>,
    custom_descriptors: Option<PathBuf>,
}

impl PipelineBuild {
    /// Board manifest path, relative to the crate root (or absolute).
    #[must_use]
    pub fn board(mut self, path: impl Into<PathBuf>) -> Self {
        self.board = Some(path.into());
        self
    }

    /// Pipeline YAML path, relative to the crate root (or absolute).
    #[must_use]
    pub fn pipeline(mut self, path: impl Into<PathBuf>) -> Self {
        self.pipeline = Some(path.into());
        self
    }

    /// A directory of custom driver/step descriptors laid out like
    /// `signal-modules` (`drivers/<id>/descriptor.yaml`, `steps/<id>/descriptor.yaml`),
    /// overlaid on the descriptors shipped with the generator.
    #[must_use]
    pub fn include(mut self, path: impl Into<PathBuf>) -> Self {
        self.custom_descriptors = Some(path.into());
        self
    }

    /// Runs codegen and writes `$OUT_DIR/pipeline.rs`. Panics on any error, as a
    /// build script should.
    pub fn generate(self) {
        let manifest_dir = manifest_dir();
        let board = resolve(
            &manifest_dir,
            self.board
                .as_deref()
                .expect("pipeline(): board manifest not set - call .board(<path>)"),
        );
        let pipeline = resolve(
            &manifest_dir,
            self.pipeline
                .as_deref()
                .expect("pipeline(): pipeline not set - call .pipeline(<path>)"),
        );

        println!("cargo:rerun-if-changed={}", board.display());
        println!("cargo:rerun-if-changed={}", pipeline.display());

        let custom = self
            .custom_descriptors
            .as_deref()
            .map(|p| resolve(&manifest_dir, p));
        if let Some(custom) = &custom {
            println!("cargo:rerun-if-changed={}", custom.display());
        }

        let source = esp_codegen::generate_esp32_embedded(&board, &pipeline, custom.as_deref())
            .unwrap_or_else(|e| panic!("Signal Layer pipeline codegen failed:\n{e:#}"));

        // The generated file opens with a crate-level `#![allow(...)]` inner
        // attribute, valid when it is a file module but not when `include!`d
        // into `mod pipeline_config { ... }`. Drop it here; the `pipeline!`
        // macro carries the allow as the module's own inner attribute.
        let source = strip_leading_inner_attrs(&source);

        let out = out_dir().join("pipeline.rs");
        std::fs::write(&out, source).unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
    }
}

/// Resolves a possibly-relative path against the crate root.
fn resolve(manifest_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest_dir.join(path)
    }
}

/// Drops leading blank lines and `#![...]` inner attributes from generated
/// source, so it can be `include!`d into a module body (where a macro-produced
/// inner attribute is illegal).
fn strip_leading_inner_attrs(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut body = false;
    for line in source.lines() {
        if !body {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("#![") {
                continue;
            }
            body = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_espflash_config(csv: &Path) {
    let path = manifest_dir().join("espflash.toml");
    if let Ok(existing) = std::fs::read_to_string(&path)
        && !existing.contains(GENERATED_MARKER)
    {
        // Someone wrote their own; theirs wins.
        return;
    }

    let contents = format!(
        "# {GENERATED_MARKER} — do not edit, do not commit.\n\
         # Points espflash at the partition table generated from partitions.toml,\n\
         # so `cargo espflash flash` enforces the same firmware ceiling the cargo\n\
         # runner does.\n\
         [idf_format_args]\n\
         partition_table = {path:?}\n",
        path = csv.display().to_string(),
    );

    if let Err(e) = std::fs::write(&path, contents) {
        println!(
            "cargo:warning=could not write {}: {e}; `cargo espflash flash` will fall back to \
             espflash's default partition table and may write an oversized image",
            path.display()
        );
    }
}

fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR is set for every build script"),
    )
}

/// The BLE host stack costs roughly half a megabyte of flash, which does not
/// fit alongside the firmware in a 2 MB app partition. The link-time guard
/// catches the overflow too, but only after a full compile; this names the
/// cause up front.
fn warn_if_ble_is_cramped(chip: &Chip, firmware_size: u64) {
    if std::env::var("CARGO_FEATURE_BLE").is_err() {
        if chip.name == "esp32c6" {
            println!(
                "cargo:warning=BLE is disabled by default for the ESP32-C6. If you wish to \
                 enable it, please use '--features ble' during compilation."
            );
        }
        return;
    }

    if firmware_size <= BLE_CRAMPED_FIRMWARE_SIZE {
        println!(
            "cargo:warning=the `ble` feature is on but firmware_size is {firmware_size:#X}; the \
             BLE host stack costs ~0.5 MB and the image is unlikely to fit. Set a larger \
             firmware_size in partitions.toml (the chip takes up to 8 MB of flash), or the \
             link will fail."
        );
    }
}

/// Emit a linker-script fragment asserting the firmware image fits the
/// `factory` (app) partition, so an oversized build fails at link time instead
/// of flashing and bricking on soft-reset (swarm#1355).
///
/// The linker cannot see the final esp-image size, so this bounds the dominant
/// term — the flash XIP extent (`.flash.appdesc` / `.rodata` / `.text`, all in
/// the `ROM` region via `REGION_ALIAS`), from `ORIGIN(ROM)` to the end of
/// `.text` (the last XIP section) — against the partition size less
/// [`FIRMWARE_IMAGE_MARGIN`] for the RAM-stored segments and image headers.
/// Conservative by design: it fails loud and early, complementing espflash's
/// flash-time `image_too_big` refusal.
fn write_firmware_size_assert(firmware_size: u64) {
    let factory_size = firmware_size - FACTORY_START;
    let limit = factory_size.saturating_sub(FIRMWARE_IMAGE_MARGIN);
    let path = out_dir().join("firmware-size-assert.x");
    let contents = format!(
        "/* @generated by esp-firmware-build — do not edit. */\n\
         ASSERT(ADDR(.text) + SIZEOF(.text) - ORIGIN(ROM) <= {limit:#X}, \
         \"firmware image overflows the {factory_size:#X}-byte app partition into the AOT region: \
         raise firmware_size / shrink aot_size in partitions.toml, or reduce code size\");\n"
    );
    std::fs::write(&path, contents)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
    println!("cargo:rustc-link-arg=-T{}", path.display());
}

/// Emit a linker-script fragment asserting the main stack's minimum size
/// (evaluated by the linker after section allocation, using the
/// `_stack_*_cpu0` symbols from esp-hal's `stack.x`).
fn write_stack_assert() {
    let path = out_dir().join("stack-size-assert.x");
    let contents = format!(
        "/* @generated by esp-firmware-build — do not edit. */\n\
         ASSERT(_stack_start_cpu0 - _stack_end_cpu0 >= {MIN_MAIN_STACK:#X}, \
         \"main stack below {MIN_MAIN_STACK:#X} bytes: .data/.bss growth has eaten the \
         leftover-RAM :( \");\n"
    );
    std::fs::write(&path, contents)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
    println!("cargo:rustc-link-arg=-T{}", path.display());
}

/// Physical layout derived from the two knobs.
struct Layout {
    meta_paddr: u64,
    meta_len: u64,
    xip_paddr: u64,
    xip_len: u64,
}

impl Layout {
    fn derive(chip: &Chip, firmware_size: u64, aot_size: u64, flash_size: u64) -> Self {
        // Alignment & sanity.
        for (label, value) in [("firmware_size", firmware_size), ("aot_size", aot_size)] {
            assert!(value > 0, "{label} must be greater than 0");
            assert!(
                value % PAGE == 0,
                "{label} ({value:#X}) must be a multiple of the {PAGE:#X} (64 KiB) flash/MMU page"
            );
        }

        let meta_paddr = firmware_size;
        let xip_paddr = meta_paddr + META_LEN;
        let end = xip_paddr + (aot_size - META_LEN);

        // Physical fit.
        assert!(
            end <= flash_size,
            "layout overflows flash on {chip}: firmware({firmware_size:#X}) \
             + aot({aot_size:#X}) reaches {end:#X}, but flash_size is {flash_size:#X}. \
             Reduce firmware_size or aot_size, or set a larger flash_size.",
            chip = chip.name,
        );

        // MMU fit: every mapped page index must stay below the
        // bootloader-reserved last page. This bounds both the non-PSRAM case
        // (offset == paddr) and the PSRAM top-page placement (which needs
        // `aot_pages` free pages below the reserved last page).
        let highest_page = end / PAGE;
        let aot_pages = aot_size / PAGE;
        let usable_pages = chip.max_page_number - 1; // last page reserved by bootloader
        assert!(
            highest_page <= usable_pages,
            "layout exceeds the MMU-addressable range on {}: highest mapped page {highest_page} > \
             usable pages {usable_pages}",
            chip.name,
        );
        assert!(
            aot_pages <= usable_pages,
            "AOT region needs {aot_pages} MMU pages but only {usable_pages} are usable on {}",
            chip.name,
        );

        Self {
            meta_paddr,
            meta_len: META_LEN,
            xip_paddr,
            xip_len: aot_size - META_LEN,
        }
    }
}

/// Detect the target chip from the `CARGO_FEATURE_ESP32C*` env vars cargo sets
/// for the enabled features. Exactly one is expected.
fn detect_chip() -> Chip {
    // (feature env suffix, chip)
    let chips = [
        (
            "C5",
            Chip {
                name: "esp32c5",
                max_page_number: esp_mmu_consts::ESP32C5_MAX_PAGE_NUMBER as u64,
            },
        ),
        (
            "C6",
            Chip {
                name: "esp32c6",
                max_page_number: esp_mmu_consts::ESP32C6_MAX_PAGE_NUMBER as u64,
            },
        ),
        (
            "C61",
            Chip {
                name: "esp32c61",
                max_page_number: esp_mmu_consts::ESP32C61_MAX_PAGE_NUMBER as u64,
            },
        ),
    ];
    let mut selected: Option<Chip> = None;
    for (suffix, chip) in chips {
        if std::env::var(format!("CARGO_FEATURE_ESP32{suffix}")).is_ok() {
            assert!(
                selected.is_none(),
                "multiple esp32c* features enabled; enable exactly one chip"
            );
            selected = Some(chip);
        }
    }
    selected.expect("no esp32c* chip feature enabled; enable one of esp32c5/c6/c61")
}

/// Deserializes a size given as a suffixed or hex string (`"2M"`, `"0x1F0000"`)
/// or as a bare integer byte count.
fn de_size<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Bytes(u64),
        Text(String),
    }

    match Option::<Raw>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Raw::Bytes(bytes)) => Ok(Some(bytes)),
        Some(Raw::Text(text)) => parse_size(&text).map(Some).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid size {text:?}: use bytes, 0x-hex, or a K/M suffix"
            ))
        }),
    }
}

/// Parse `"2M"`, `"1984K"`, `"0x1F0000"`, or a plain decimal byte count.
fn parse_size(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    let lower = s.to_ascii_lowercase();
    let (num, mult) = if let Some(n) = lower.strip_suffix("mb").or_else(|| lower.strip_suffix('m'))
    {
        (n, 1024 * 1024)
    } else if let Some(n) = lower.strip_suffix("kb").or_else(|| lower.strip_suffix('k')) {
        (n, 1024)
    } else {
        (lower.as_str(), 1)
    };
    num.trim().parse::<u64>().ok().map(|v| v * mult)
}

/// Emits the layout as a bare expression, so `partition_layout!()` can
/// `include!` it in expression position.
fn write_layout_rs(layout: &Layout) {
    let path = out_dir().join("esp_firmware_partition_layout.rs");
    let contents = format!(
        "// @generated by esp-firmware-build from partitions.toml — do not edit.\n\
         ::esp_firmware::PartitionLayout {{\n\
         \x20   meta_paddr: {},\n\
         \x20   meta_len: {},\n\
         \x20   xip_paddr: {},\n\
         \x20   xip_len: {},\n\
         }}\n",
        hex(layout.meta_paddr),
        hex(layout.meta_len),
        hex(layout.xip_paddr),
        hex(layout.xip_len),
    );
    std::fs::write(&path, contents)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
}

/// Format a value as an underscore-grouped hex literal (e.g. `0x1F_0000`) so
/// the generated code satisfies clippy's `unreadable_literal` lint.
fn hex(value: u64) -> String {
    let digits = format!("{value:X}");
    let mut grouped = String::new();
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 4 == 0 {
            grouped.push('_');
        }
        grouped.push(c);
    }
    format!("0x{grouped}")
}

/// Emit the app-only partition CSV consumed by espflash, returning its path.
///
/// It lives in `OUT_DIR` rather than the source tree; the build script re-runs
/// whenever the layout could change, so the `espflash.toml` pointing at it is
/// refreshed in the same pass.
fn write_partitions_csv(layout: &Layout, firmware_size: u64) -> std::path::PathBuf {
    let path = out_dir().join("partitions.generated.csv");
    let aot_end = layout.xip_paddr + layout.xip_len;
    let contents = format!(
        "# @generated by esp-firmware-build from partitions.toml — do not edit, do not commit.\n\
         # App-only ESP-IDF partition table. The AOT XIP region\n\
         # ({meta:#X}..{end:#X}) is intentionally left UNPARTITIONED: it is mapped\n\
         # manually by esp-mmu and mutated at runtime, so the bootloader must not\n\
         # map or validate it.\n\
         # Name,     Type, SubType, Offset,    Size\n\
         nvs,        data, nvs,     0x9000,    0x6000\n\
         phy_init,   data, phy,     0xf000,    0x1000\n\
         factory,    app,  factory, {factory_start:#x},  {fw:#x}\n",
        meta = layout.meta_paddr,
        end = aot_end,
        factory_start = FACTORY_START,
        fw = firmware_size - FACTORY_START,
    );
    std::fs::write(&path, contents)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
    path
}

fn out_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("OUT_DIR").expect("OUT_DIR is set for every build script"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: u64 = 1024 * 1024;

    fn layout(firmware: u64, aot: u64, flash: u64) -> Partitions {
        Partitions {
            firmware_size: Some(firmware),
            aot_size: Some(aot),
            flash_size: Some(flash),
        }
    }

    #[test]
    fn compact_form_round_trips_every_knob() {
        let full = layout(2 * M, 2 * M, 4 * M);
        assert_eq!(Partitions::parse_compact(&full.to_compact()).unwrap(), full);
    }

    #[test]
    fn compact_form_carries_only_the_knobs_that_are_set() {
        let partial = Partitions {
            flash_size: Some(8 * M),
            ..Partitions::default()
        };
        assert_eq!(partial.to_compact(), "flash=0x800000");
        assert_eq!(
            Partitions::parse_compact("flash=0x800000").unwrap(),
            partial
        );
    }

    #[test]
    fn compact_form_rejects_unknown_keys_and_unparseable_sizes() {
        let err = Partitions::parse_compact("bogus=1M").unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        let err = Partitions::parse_compact("flash=lots").unwrap_err();
        assert!(err.contains("lots"), "{err}");
        assert!(Partitions::parse_compact("flash").is_err());
    }

    #[test]
    fn read_partitions_toml_is_none_when_the_crate_has_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_partitions_toml(dir.path()).unwrap(), None);
    }

    #[test]
    fn read_partitions_toml_reads_the_partitions_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("partitions.toml"),
            "[partitions]\nflash_size = \"8M\"\n",
        )
        .unwrap();
        assert_eq!(
            read_partitions_toml(dir.path()).unwrap(),
            Some(Partitions {
                flash_size: Some(8 * M),
                ..Partitions::default()
            })
        );
    }

    #[test]
    fn read_partitions_toml_names_the_file_in_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("partitions.toml"), "[partitions\n").unwrap();
        let err = read_partitions_toml(dir.path()).unwrap_err();
        assert!(err.contains("partitions.toml"), "{err}");
    }

    #[test]
    fn default_layout_without_a_flash_size_is_the_4m_split() {
        assert_eq!(Partitions::default_layout(None), layout(3 * M, M, 4 * M));
    }

    #[test]
    fn default_layout_with_a_known_flash_size_keeps_aot_and_gives_the_rest_to_firmware() {
        assert_eq!(
            Partitions::default_layout(Some(8 * M)),
            layout(7 * M, M, 8 * M)
        );
    }

    #[test]
    fn toml_sizes_accept_suffixed_strings_hex_strings_and_bare_integers() {
        let parsed: Partitions = toml::from_str(
            "firmware_size = \"2M\"\naot_size = \"0x200000\"\nflash_size = 4194304\n",
        )
        .unwrap();
        assert_eq!(parsed, layout(2 * M, 2 * M, 4 * M));
    }

    #[test]
    fn partitions_toml_beats_the_env_which_beats_the_built_in_default() {
        let file = layout(4 * M, 2 * M, 8 * M);
        let env = "flash=0x1000000";
        assert_eq!(Partitions::select(Some(file.clone()), Some(env)), file);
        assert_eq!(
            Partitions::select(None, Some(env)),
            Partitions {
                flash_size: Some(16 * M),
                ..Partitions::default()
            }
        );
        assert_eq!(Partitions::select(None, None), Partitions::default());
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;

    const M: u64 = 1024 * 1024;

    #[test]
    fn display_spells_the_set_knobs_the_way_partitions_toml_does() {
        let full = Partitions {
            firmware_size: Some(2 * M),
            aot_size: Some(1984 * 1024),
            flash_size: Some(0x3F_1234),
        };
        assert_eq!(
            full.to_string(),
            "firmware_size = \"2M\", aot_size = \"1984K\", flash_size = \"0x3F1234\""
        );
        let partial = Partitions {
            flash_size: Some(8 * M),
            ..Partitions::default()
        };
        assert_eq!(partial.to_string(), "flash_size = \"8M\"");
    }
}
