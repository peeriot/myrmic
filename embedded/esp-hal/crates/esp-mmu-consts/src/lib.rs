//! Dependency-free ESP MMU hardware constants.
//!
//! These are the single source of truth for per-chip MMU parameters, split out of `esp-mmu` so
//! they can be shared with host tooling. `esp-mmu` pulls in `esp-hal` and only builds for the
//! target, so a host build script (e.g. `modem-esp32`'s partition-layout generator) cannot read
//! `esp_mmu::Mmu::MAX_PAGE_NUMBER` directly — it reads the matching constant from here instead,
//! while `esp-mmu` re-exports/uses these same values on target.
#![no_std]
#![warn(missing_docs)]

/// Highest MMU page count on the ESP32-C5.
pub const ESP32C5_MAX_PAGE_NUMBER: usize = 512;
/// Highest MMU page count on the ESP32-C6.
pub const ESP32C6_MAX_PAGE_NUMBER: usize = 256;
/// Highest MMU page count on the ESP32-C61.
pub const ESP32C61_MAX_PAGE_NUMBER: usize = 512;
