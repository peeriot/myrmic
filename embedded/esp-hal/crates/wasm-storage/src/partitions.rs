//! Partitions for the supported hardware
//!
//! The physical placement of the two AOT flash regions (metadata + XIP module) is described by
//! [`PartitionLayout`]. A crate can then use this layout to provide a custom sized layout to allow
//! for firmware vs. WASM storage tuning.
//!
//! *NOTE*: The AOT regions are deliberately **not** ESP-IDF partitions — they are mapped manually
//! by `esp-mmu` at raw flash offsets and their content is mutated at runtime, so the bootloader
//! must neither map nor validate them. The generated partition table only declares the app
//! (`factory`) partition and leaves this region as an unpartitioned gap.

/// Physical (and derived virtual) layout of the two AOT flash regions.
///
/// * The metadata region stores the [`crate::metadata::Metadata`] describing the stored module.
/// * The XIP region stores the AOT module itself, executed in place directly from flash.
///
/// The metadata and XIP regions are contiguous: `xip_paddr == meta_paddr + meta_len`.
///
/// Both regions can be written externally with the `espflash` utility, e.g. for an ESP32-C6 with
/// the default layout:
/// ```no_run
/// $ espflash write-bin 0x200000 aot_meta.bin
/// $ espflash write-bin 0x210000 wasm_module.aot
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionLayout {
    /// Physical Flash address of the metadata region
    pub meta_paddr: usize,
    /// Length in bytes of the metadata region
    pub meta_len: usize,
    /// Physical Flash address of the XIP module region
    pub xip_paddr: usize,
    /// Length in bytes of the XIP module region
    pub xip_len: usize,
}

impl PartitionLayout {
    /// Default layout used when no custom partition scheme is provided.
    pub const DEFAULT: Self = Self {
        meta_paddr: 0x20_0000,
        meta_len: 0x1_0000,
        xip_paddr: 0x21_0000,
        xip_len: 0x1F_0000,
    };
}

impl Default for PartitionLayout {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// For chips that don't use PSRAM, we can simply use the `paddr` as the virtual offset. However, if
// the PSRAM is used, this cannot be true. PSRAM and XIP use the same MMU page space. PSRAM occupies
// the lowest non-used pages after boot. If we were to also just use paddr as an offset the
// subsequent XIP mapping would collide with the PSRAM virtual address if the PSRAM is big enough.
// What we need to do instead, is to make sure that we use the highest MMU pages for XIP to avoid any
// collisions with PSRAM. We also need to make sure not to use the last page, because that is
// reserved to the bootloader.

// non-PSRAM SoCs: the virtual offset is just the physical address.
#[cfg(feature = "esp32c6")]
impl PartitionLayout {
    /// Virtual-address offset at which the metadata region is mapped.
    pub(crate) fn meta_vaddr_offset(&self) -> usize {
        self.meta_paddr
    }

    /// Virtual-address offset at which the XIP region is mapped.
    pub(crate) fn xip_vaddr_offset(&self) -> usize {
        self.xip_paddr
    }
}

// PSRAM SoCs: place both regions at the highest MMU pages to avoid colliding with PSRAM (which
// occupies the lowest pages) and with the bootloader's reserved last page.
#[cfg(any(feature = "esp32c5", feature = "esp32c61"))]
impl PartitionLayout {
    /// MMU page size assumed for XIP placement (flash-sector granularity).
    const MMU_PAGE: usize = 0x1_0000;

    /// Highest MMU page usable by the app (the last page is reserved by the bootloader).
    fn max_unreserved_page() -> usize {
        esp_mmu::Mmu::MAX_PAGE_NUMBER - 1
    }

    /// Virtual-address offset at which the metadata region is mapped.
    pub(crate) fn meta_vaddr_offset(&self) -> usize {
        (Self::max_unreserved_page()
            - (self.xip_len / Self::MMU_PAGE)
            - (self.meta_len / Self::MMU_PAGE))
            * Self::MMU_PAGE
    }

    /// Virtual-address offset at which the XIP region is mapped.
    pub(crate) fn xip_vaddr_offset(&self) -> usize {
        (Self::max_unreserved_page() - (self.xip_len / Self::MMU_PAGE)) * Self::MMU_PAGE
    }
}
