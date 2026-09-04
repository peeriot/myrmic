//! Build script: turns the optional `partitions.toml` into the AOT flash layout consumed by
//! `wasm-storage` and the app partition table consumed by `espflash`.
//!
//! Two knobs are exposed to the integrator — the firmware (app) partition size and the AOT XIP
//! storage size. From those (plus the target chip and its flash size) this script derives a fully
//! contiguous physical layout, validates it, and emits into `$OUT_DIR`:
//!
//! * `partition_layout.rs` — a `wasm_storage::PartitionLayout` const, `include!`d by `main.rs` and
//!   passed to `WasmStorage::new`.
//! * `partitions.generated.csv` — an app-only ESP-IDF partition table (`nvs` / `phy_init` /
//!   `factory`).
//!   The AOT region is deliberately left as an unpartitioned gap: it is mapped manually by
//!   `esp-mmu` and mutated at runtime, so the bootloader must neither map nor validate it. Sizing
//!   `factory` to end exactly at the AOT boundary makes `espflash` refuse to flash a firmware image
//!   that would overflow into the AOT region — enforcing the ceiling with the stock bootloader.
//!
//! When no `partitions.toml` is present, the historical 4 MB layout is used, so behaviour is
//! unchanged out of the box.

use std::path::Path;

use serde::Deserialize;

/// MMU / flash mapping granularity. All regions are aligned to this.
const PAGE: u64 = 0x1_0000; // 64 KiB
/// Metadata region size (always one page).
const META_LEN: u64 = PAGE;
/// Start of the `factory` (app) partition — right after bootloader, partition table, `nvs` and
/// `phy_init`.
const FACTORY_START: u64 = 0x1_0000;
/// Recommended minimum `aot_size`. Below this a usable WASM module is unlikely to fit, so we emit a
/// cargo warning (but still allow the build).
const RECOMMENDED_MIN_AOT_SIZE: u64 = 200 * 1024;

/// Flash reserved, on top of the XIP extent the firmware-size guard measures, for the parts of the
/// esp-image the linker cannot see: the RAM-loaded segments (`.data`/`.rwtext`, stored in the image
/// and copied to RAM at boot) and the esp-image / per-segment headers. Sized to cover a wifi+ble
/// build's RAM-stored segments with slack, so the guard stays a conservative link-time bound
/// (swarm#1355).
const FIRMWARE_IMAGE_MARGIN: u64 = 0x3_0000; // 192 KiB

/// Minimum main-stack size, enforced at link time. esp-hal sizes `.stack` as whatever RWDATA is
/// left after `.data`/`.bss`, so unrelated static growth shrinks it silently (the `ble` feature
/// costs ~39 `KiB` of `.bss` and leaves ~8 `KiB` of stack).
///
/// The deep zenoh/db poll chains run on their own fixed-size thread stack (`NET_STACK` in
/// main.rs), so the main stack only carries the shallow tasks (~6 `KiB` peak via `stack-hwm`).
/// A build whose leftover drops below this fails loudly instead of overflowing into `.bss`.
const MIN_MAIN_STACK: u64 = 0x1F00;

/// Per-chip constraints.
struct Chip {
    /// Feature name, used only for diagnostics.
    name: &'static str,
    /// Highest MMU page count.
    max_page_number: u64,
}

/// Raw config file schema. Every field is optional; a missing file or key falls back to defaults.
#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    partitions: Partitions,
}

/// The partition knobs
#[derive(Deserialize, Default)]
struct Partitions {
    /// Firmware partition size (includes bootloader) (e.g. `"2M"`, `"0x1F0000"`, `2031616`).
    firmware_size: Option<String>,
    /// AOT XIP storage size. It includes both a 64 KB metadata region and the AOT XIP module.
    aot_size: Option<String>,
    /// Total usable flash on the target device. Defaults to 4M.
    flash_size: Option<String>,
}

fn main() {
    let chip = detect_chip();

    if cfg!(all(feature = "esp32c6", not(feature = "ble"))) {
        println!(
            "cargo:warning=BLE is disabled by default for the ESP32-C6. If you wish to enable it, \
            please use '--features ble' during compilation."
        );
    }

    let config_path =
        Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("partitions.toml");
    println!("cargo:rerun-if-changed={}", config_path.display());

    let config: Config = if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", config_path.display()));
        toml::from_str(&text)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", config_path.display()))
    } else {
        Config::default()
    };

    // Defaults to a 4 MB layout (3M `firmware_size` + 1M `aot_size`, see #1347).
    let default_size = 0x40_0000;
    let firmware_size = resolve(
        config.partitions.firmware_size.as_deref(),
        0x30_0000,
        "firmware_size",
    );
    let aot_size = resolve(config.partitions.aot_size.as_deref(), 0x10_0000, "aot_size");
    let flash_size = resolve(
        config.partitions.flash_size.as_deref(),
        default_size,
        "flash_size",
    );

    if aot_size < RECOMMENDED_MIN_AOT_SIZE {
        println!(
            "cargo:warning=aot_size ({aot_size:#X}) is below the recommended minimum of {RECOMMENDED_MIN_AOT_SIZE:#X} \
             ({} KiB); a usable WASM module is unlikely to fit",
            RECOMMENDED_MIN_AOT_SIZE / 1024,
        );
    }

    let layout = Layout::derive(&chip, firmware_size, aot_size, flash_size);

    write_layout_rs(&layout);
    write_partitions_csv(&layout, firmware_size);
    write_stack_assert();
    write_firmware_size_assert(firmware_size);
}

/// Emit a linker-script fragment asserting the firmware image fits the `factory` (app) partition,
/// so an oversized build fails at link time instead of flashing and bricking on soft-reset
/// (swarm#1355, the build-time complement to the partition resize in #1347).
///
/// The linker cannot see the final esp-image size, so this bounds the dominant term — the flash XIP
/// extent (`.flash.appdesc` / `.rodata` / `.text`, all in the `ROM` region via `REGION_ALIAS`), from
/// `ORIGIN(ROM)` to the end of `.text` (the last XIP section) — against the partition size less
/// [`FIRMWARE_IMAGE_MARGIN`] for the RAM-stored segments and image headers. Conservative by design:
/// it fails loud and early, complementing espflash's flash-time `image_too_big` refusal.
fn write_firmware_size_assert(firmware_size: u64) {
    let factory_size = firmware_size - FACTORY_START;
    let limit = factory_size.saturating_sub(FIRMWARE_IMAGE_MARGIN);
    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("firmware-size-assert.x");
    let contents = format!(
        "/* @generated by build.rs — do not edit. */\n\
         ASSERT(ADDR(.text) + SIZEOF(.text) - ORIGIN(ROM) <= {limit:#X}, \
         \"firmware image overflows the {factory_size:#X}-byte app partition into the AOT region: \
         raise firmware_size / shrink aot_size in partitions.toml, or reduce code size\");\n"
    );
    std::fs::write(&path, contents)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
    println!("cargo:rustc-link-arg=-T{}", path.display());
}

/// Emit a linker-script fragment asserting the main stack's minimum size (evaluated by the linker
/// after section allocation, using the `_stack_*_cpu0` symbols from esp-hal's `stack.x`).
fn write_stack_assert() {
    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("stack-size-assert.x");
    let contents = format!(
        "/* @generated by build.rs — do not edit. */\n\
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

        // MMU fit: every mapped page index must stay below the bootloader-reserved last page. This
        // bounds both the non-PSRAM case (offset == paddr) and the PSRAM top-page placement (which
        // needs `aot_pages` free pages below the reserved last page).
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

/// Detect the target chip from the `CARGO_FEATURE_ESP32C*` env vars cargo sets for the enabled
/// features. Exactly one is expected.
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

/// Resolve an optional size string to bytes, falling back to `default`.
fn resolve(value: Option<&str>, default: u64, label: &str) -> u64 {
    match value {
        Some(s) => parse_size(s).unwrap_or_else(|| {
            panic!("invalid {label} value {s:?}: use bytes, 0x-hex, or a K/M suffix")
        }),
        None => default,
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

/// Emits the `PartitionLayout` const to be included in `main.rs`.
fn write_layout_rs(layout: &Layout) {
    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("partition_layout.rs");
    let contents = format!(
        "// @generated by build.rs from partitions.toml — do not edit.\n\
         pub const PARTITION_LAYOUT: ::wasm_storage::PartitionLayout = ::wasm_storage::PartitionLayout {{\n\
         \x20   meta_paddr: {},\n\
         \x20   meta_len: {},\n\
         \x20   xip_paddr: {},\n\
         \x20   xip_len: {},\n\
         }};\n",
        hex(layout.meta_paddr),
        hex(layout.meta_len),
        hex(layout.xip_paddr),
        hex(layout.xip_len),
    );
    std::fs::write(&path, contents)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
}

/// Format a value as an underscore-grouped hex literal (e.g. `0x1F_0000`) so the generated code
/// satisfies clippy's `unreadable_literal` lint.
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

/// Emit the app-only partition CSV consumed by espflash.
fn write_partitions_csv(layout: &Layout, firmware_size: u64) {
    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("partitions.generated.csv");
    let aot_end = layout.xip_paddr + layout.xip_len;
    let contents = format!(
        "# @generated by build.rs from partitions.toml — do not edit, do not commit.\n\
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
}
