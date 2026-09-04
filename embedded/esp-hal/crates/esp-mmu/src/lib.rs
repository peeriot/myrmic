//! ESP MMU Driver
//!
//! This crate implements an MMU (Memory Management Unit) driver that can run in a `esp-hal`
//! ecosystem. Allows for mapping and unmapping virtual CPU addresses to physical SPI flash
//! addresses so that data/instructions are not limited to just Flash read (which consumes RAM) but
//! uses the CPU to directly address areas of flash (read-only) as slices.
//!
//! This tries to replicate the ability that ESP-IDF has to call `esp_partition_mmap` and
//! `esp_partition_munmap` to dynamically load partitions.
#![no_std]
#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![warn(unreachable_pub)]
#![warn(clippy::must_use_candidate)]
#![warn(clippy::return_self_not_must_use)]
#![expect(
    unused_parens,
    reason = "Dictated by how to use cfg_match! in some cases"
)]

use core::fmt::{Display, LowerHex, UpperHex};

use cfg_match::cfg_match;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use esp_hal::ram;

#[cfg(feature = "esp32c6")]
use esp_hal::peripherals::PLIC_MX;
#[cfg(any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61"))]
use esp_hal::peripherals::SPI0;

/// Macro that can be used to gain ownership of the required peripherals to create an instance of
/// [`Mmu`].
///
/// The macro allows to gain partial ownership of the ESP peripherals, letting the Rust borrow
/// checker making sure to gain unique access to the required peripherals, while still letting the
/// user use the rest of the fields of the `Peripherals` structure for other purposes.
///
/// This macro handles automatically the support for different hardwares.
///
/// # Example Usage
///
/// ```no_run
/// let mmu = mmu_from_peripherals!(peripherals);
/// ```
#[macro_export]
macro_rules! mmu_from_peripherals {
    ($periph:ident) => {{
        let mmu = $crate::__reexports::cfg_match::cfg_match! {
            feature = "esp32c6" => $crate::Mmu::new($periph.SPI0, $periph.PLIC_MX),
            any(feature = "esp32c5", feature = "esp32c61") => $crate::Mmu::new($periph.SPI0),
            _ => unimplemented!(),
        };

        mmu
    }};
}

/// Kinds of MMU errors
#[derive(Debug)]
pub enum Error {
    /// Addresses are unaligned
    Alignment,
    /// Addresses are invalid
    OutOfRange,
}

/// Representation of a Virtual CPU address
#[derive(Debug, Copy, Clone)]
pub enum VirtualAddress {
    /// Address uses BUS to access data as storage
    Data(usize),
    /// Address uses BUS to access data as executable instructions
    Instruction(usize),
}

impl VirtualAddress {
    /// Returns just the address value of this virtual address
    #[must_use]
    pub fn address(&self) -> usize {
        match self {
            VirtualAddress::Data(addr) | VirtualAddress::Instruction(addr) => *addr,
        }
    }
}

impl Display for VirtualAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Data(addr) | Self::Instruction(addr) => Display::fmt(&addr, f),
        }
    }
}

impl UpperHex for VirtualAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Data(addr) | Self::Instruction(addr) => UpperHex::fmt(&addr, f),
        }
    }
}

impl LowerHex for VirtualAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Data(addr) | Self::Instruction(addr) => LowerHex::fmt(&addr, f),
        }
    }
}

/// A Flash Mapped region
pub struct Region<'a, R: RawMutex> {
    /// Protected acccess to the MMU functionality
    mmu: &'a Mutex<R, Mmu>,
    /// Starting virtual address of the region
    vaddr: VirtualAddress,
    /// Physical Flash mapped address
    pub paddr: usize,
    /// Size of the region
    pub size: usize,
}

impl<R: RawMutex> core::fmt::Debug for Region<'_, R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Region")
            .field("vaddr", &self.vaddr)
            .field("paddr", &self.paddr)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl<'a, R: RawMutex> Region<'a, R> {
    /// Creates a new region mapping a CPU virtual address to a Flash physical address
    ///
    /// # Errors
    ///
    /// Returns errors if the addresses or sizes are out of range or misaligned
    pub fn new(
        mmu: &'a Mutex<R, Mmu>,
        vaddr: VirtualAddress,
        paddr: usize,
        size: usize,
    ) -> Result<Self, Error> {
        mmu.lock(|mmu| mmu.map_region(vaddr, paddr, size))?;

        Ok(Self {
            mmu,
            vaddr,
            paddr,
            size,
        })
    }

    /// Obtains access to the region as a slice directly addressing data without having to first
    /// read it to RAM
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // safety: address and size were already validated by `mmu.map_region(_)` in `new`
        unsafe { core::slice::from_raw_parts(self.vaddr.address() as *const u8, self.size) }
    }
}

impl<R: RawMutex> Drop for Region<'_, R> {
    fn drop(&mut self) {
        if self
            .mmu
            .lock(|mmu| mmu.unmap_region(self.vaddr, self.size))
            .is_err()
        {
            log::warn!(
                "Failed to unmap region {:#0X} {:#0X}",
                self.vaddr,
                self.size
            );
        }
    }
}

/// Memory Management Unit
#[derive(Debug)]
pub struct Mmu {
    /// SPI0 which has MMU functionality and is used to communicate with external flash
    #[cfg(any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61"))]
    spi0: SPI0<'static>,
    /// Interrupt controller
    #[cfg(feature = "esp32c6")]
    plic_mx: PLIC_MX<'static>,
}

impl Mmu {
    /// Determines the highest Physical address supported by the MMU
    #[ram]
    pub const MAX_PAGE_NUMBER: usize = cfg_match! {
        feature = "esp32c6" => esp_mmu_consts::ESP32C6_MAX_PAGE_NUMBER,
        feature = "esp32c5" => esp_mmu_consts::ESP32C5_MAX_PAGE_NUMBER,
        feature = "esp32c61" => esp_mmu_consts::ESP32C61_MAX_PAGE_NUMBER,
        _ => unimplemented!(),
    };

    /// Creates a new instance of a MMU Peripheral
    #[cfg(feature = "esp32c6")]
    #[must_use]
    pub fn new(spi0: SPI0<'static>, plic_mx: PLIC_MX<'static>) -> Self {
        Self { spi0, plic_mx }
    }

    /// Creates a new instance of a MMU Peripheral
    #[cfg(any(feature = "esp32c5", feature = "esp32c61"))]
    #[must_use]
    pub fn new(spi0: SPI0<'static>) -> Self {
        Self { spi0 }
    }

    /// Returns the size of an MMU page in bytes
    #[cfg_attr(any(feature = "esp32c5"), allow(clippy::unused_self))]
    #[ram]
    fn mmu_page_size(&self) -> usize {
        cfg_match! {
            any(feature = "esp32c5") => 64 * 1_024,
            any(feature = "esp32c6", feature = "esp32c61") => ({
                let regs = self.spi0.register_block();
                let mmu_page_size_bits = cfg_match! {
                    feature = "esp32c6" => regs.mmu_power_ctrl().read().spi_mmu_page_size().bits(),
                    feature = "esp32c61" => regs.mmu_power_ctrl().read().mmu_page_size().bits(),
                };
                match mmu_page_size_bits {
                    0 => 64 * 1_024,
                    1 => 32 * 1_024,
                    2 => 16 * 1_024,
                    3 => 8 * 1_024,
                    #[expect(clippy::unreachable, reason = "Hardware dictates it")]
                    _ => unreachable!("Hardware doesn't allow any other value"),
                }
            }),
            _ => unimplemented!(),
        }
    }

    /// Helper function to operate with a disabled cache
    ///
    /// This allows to avoid cached access to flash during mapping/unmapping
    #[ram]
    pub fn uncached_section<R>(&self, vaddr: usize, len: usize, f: impl FnOnce() -> R) -> R {
        // Stop Cache
        // Disable non IRAM interrupts (remember them for later so we can re-enable them)
        let saved_interrupts = cfg_match! {
            feature = "esp32c6" => ({
                let plic_mx_regs = self.plic_mx.register_block();
                let saved_interrupts = plic_mx_regs.mxint_enable().read().bits();
                plic_mx_regs.mxint_enable().write(|w| {
                    // safety: we are setting the correct hardware bits
                    unsafe { w.bits(0) }
                });
                saved_interrupts
            }),
            any(feature = "esp32c5", feature = "esp32c61") => ({
                // C5/C61 uses a CLIC: no single interrupt-enable register exists. So the external
                // lines (the internal lines are reserved for CLINT) need to be independently
                // disabled (by controlling the `int_ie` bit).
                // Each external line N has its own control word and the enable bit (CLIC_INT_IE)
                // is isolated in byte 1 of that word. External lines start at CLIC index 16.
                let clic = {
                    // safety: exclusive access to the CLIC during the uncached section
                    #[cfg(feature = "esp32c5")]
                    unsafe {
                        esp32c5::CLIC::steal()
                    }
                    // safety: exclusive access to the CLIC during the uncached section
                    #[cfg(feature = "esp32c61")]
                    unsafe {
                        esp32c61::CLIC::steal()
                    }
                };
                let mut saved_interrupts: u32 = 0;
                // Save enabled state as u32 bitflag
                for (idx, line) in clic.int_ie_iter().enumerate() {
                    // Save state into u32 bitflag
                    if line.read().int_ie().bit_is_set() {
                        saved_interrupts |= 1 << idx;
                        // Disable interrupt
                        line.write(|w| w.int_ie().clear_bit());
                    }
                }
                saved_interrupts
            }),
            _ => unimplemented!(),
        };

        // Freeze the cache for the duration of the MMU rewrite. The contract above
        // requires "no cache" while the MMU is reprogrammed; without the freeze the
        // entries are rewritten under a live cache, and only the newly mapped range
        // is invalidated afterwards, so any line disturbed outside it stays stale.
        // safety: ROM functions can be used in this context, and everything executed
        // until `cache_start` below is `#[ram]`-resident, so no flash fetch happens
        // while the cache is frozen.
        unsafe {
            cache_stop();
        }

        let res = f();

        // Invalidate cache (we just mounted so this makes sure that the CPU doesn't hold onto any
        // previous past assumptions about this virtual address range)
        // safety: ROM functions can be used in this context
        unsafe {
            cache_invalidate_addr(vaddr, len);
        }

        // Start Cache
        // safety: ROM functions can be used in this context
        unsafe {
            cache_start();
        }
        cfg_match! {
            feature = "esp32c6" => ({
                let plic_mx_regs = self.plic_mx.register_block();
                plic_mx_regs
                    .mxint_enable()
                    .write(|w| {
                        // safety: we are setting the correct hardware bits
                        unsafe { w.bits(saved_interrupts) }
                    });
            }),
            any(feature = "esp32c5", feature = "esp32c61") => ({
                let clic = {
                    // safety: exclusive access to the CLIC during the uncached section
                    #[cfg(feature = "esp32c5")]
                    unsafe {
                        esp32c5::CLIC::steal()
                    }
                    // safety: exclusive access to the CLIC during the uncached section
                    #[cfg(feature = "esp32c61")]
                    unsafe {
                        esp32c61::CLIC::steal()
                    }
                };
                // Restore enabled state as u32 bitflag
                for (idx, line) in clic.int_ie_iter().enumerate() {
                    if saved_interrupts & (1 << idx) != 0 {
                        line.write(|w| w.int_ie().set_bit());
                    }
                }
            }),
            _ => unimplemented!(),
        }

        res
    }

    /// Maps a region of virtual CPU address to physical Flash address
    ///
    /// # Safety
    ///
    /// In order for this operation to be valid, only map unmapped regions (don't map any region
    /// that is used by the bootloader by the "app")
    ///
    /// # Errors
    ///
    /// Errors if:
    /// * The addresses or len are unaligned
    /// * The region is not valid
    // Important to run as RAM function. When mounting and operating on cache, any access to
    // external flash is forbidden
    #[expect(
        clippy::cast_possible_truncation,
        reason = "We are running only on 32-bit targets"
    )]
    #[ram]
    pub(crate) fn map_region(
        &self,
        vaddr: VirtualAddress,
        paddr: usize,
        len: usize,
    ) -> Result<(), Error> {
        log::trace!("map_region vaddr={vaddr:#0X} paddr={paddr:#0X} len={len:#0X}");

        // Make sure vaddr and paddr are valid
        let addressable_size = Self::MAX_PAGE_NUMBER * self.mmu_page_size();
        let mut vaddr = match vaddr {
            VirtualAddress::Data(d_addr) => {
                let vaddr_range = DBUS_BASE..DBUS_BASE + addressable_size;
                let paddr_range = 0..addressable_size;
                if vaddr_range.contains(&d_addr)
                    && vaddr_range.contains(&(d_addr + len))
                    && paddr_range.contains(&paddr)
                    && paddr_range.contains(&(paddr + len))
                {
                    d_addr
                } else {
                    return Err(Error::OutOfRange);
                }
            }
            VirtualAddress::Instruction(i_addr) => {
                let vaddr_range = IBUS_BASE..IBUS_BASE + addressable_size;
                let paddr_range = 0..addressable_size;
                if vaddr_range.contains(&i_addr)
                    && vaddr_range.contains(&(i_addr + len))
                    && paddr_range.contains(&paddr)
                    && paddr_range.contains(&(paddr + len))
                {
                    i_addr
                } else {
                    return Err(Error::OutOfRange);
                }
            }
        };

        // Make sure addresses and lengths are aligned
        if !vaddr.is_multiple_of(self.mmu_page_size())
            || !paddr.is_multiple_of(self.mmu_page_size())
            || !len.is_multiple_of(self.mmu_page_size())
        {
            return Err(Error::Alignment);
        }

        // Calculate number of pages required for mapping (round up)
        let page_size = self.mmu_page_size();
        let number_of_pages = len.div_ceil(page_size);
        let mapped_len = number_of_pages * page_size;

        // Run mapping in critical section
        //  It's important that while we operate on the MMU we have no interrupts, no FLASH access,
        //  no FLASH interrupts, no cache.
        critical_section::with(|_| {
            self.uncached_section(vaddr, mapped_len, || {
                let shift_code = cfg_match! {
                    any(feature = "esp32c5") => 16,
                    any(feature = "esp32c6", feature = "esp32c61") => ({
                        if page_size == 64 * 1_024 {
                            16
                        } else if page_size == 32 * 1_024 {
                            15
                        } else if page_size == 16 * 1_024 {
                            14
                        } else if page_size == 8 * 1_024 {
                            13
                        } else {
                            #[expect(clippy::unreachable, reason = "Hardware dictates it")]
                            {unreachable!("Hardware doesn't allow any other value")}
                        }
                    }),
                    _ => unimplemented!(),
                };
                let mut paddr_mmu_formatted = paddr >> shift_code;

                let vaddr_mask = cfg_match! {
                    any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => (page_size * Self::MAX_PAGE_NUMBER) - 1,
                    _ => unimplemented!(),
                };

                #[expect(clippy::explicit_counter_loop, reason = "False positive")]
                for _ in 0..number_of_pages {
                    let entry_id = (vaddr & vaddr_mask) >> shift_code;
                    cfg_match! {
                        any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => ({
                            let spi0_regs = self.spi0.register_block();

                            // Write entry id to select
                            spi0_regs
                                .mmu_item_index()
                                .write(|w| {
                                // safety: we are setting the correct hardware bits
                                unsafe {
                                    w.bits(entry_id as u32)
                                }
                            });
                            // Write contents of the mapping
                            spi0_regs
                                .mmu_item_content()
                                .write(|w| {
                                // safety: we are setting the correct hardware bits
                                unsafe {
                                    w.bits(paddr_mmu_formatted as u32 | VALID_BIT)
                                }
                            });
                        }),
                        _ => unimplemented!(),
                    }

                    vaddr += page_size;
                    paddr_mmu_formatted += 1;
                }

                Ok(())
            })
        })
    }

    /// Unmaps a previously mapped region
    ///
    /// Unmapping is safe to perform on already unmapped regions.
    ///
    /// # Errors
    ///
    /// Returns an error if the address or the length are out of range or misaligned
    // Important to run as RAM function. When mounting and operating on cache, any access to
    // external flash is forbidden
    #[ram]
    pub fn unmap_region(&self, vaddr: VirtualAddress, len: usize) -> Result<(), Error> {
        log::trace!("unmap_region vaddr={vaddr:#0X} len={len:#0X}");

        // Make sure vaddr is valid
        let addressable_size = Self::MAX_PAGE_NUMBER * self.mmu_page_size();
        let mut vaddr = match vaddr {
            VirtualAddress::Data(d_addr) => {
                let vaddr_range = DBUS_BASE..DBUS_BASE + addressable_size;
                if vaddr_range.contains(&d_addr) && vaddr_range.contains(&(d_addr + len)) {
                    d_addr
                } else {
                    return Err(Error::OutOfRange);
                }
            }
            VirtualAddress::Instruction(i_addr) => {
                let vaddr_range = IBUS_BASE..IBUS_BASE + addressable_size;
                if vaddr_range.contains(&i_addr) && vaddr_range.contains(&(i_addr + len)) {
                    i_addr
                } else {
                    return Err(Error::OutOfRange);
                }
            }
        };

        // Make sure addresses and lengths are aligned
        if !vaddr.is_multiple_of(self.mmu_page_size()) || !len.is_multiple_of(self.mmu_page_size())
        {
            return Err(Error::Alignment);
        }

        // Calculate number of mapped pages (round up)
        let page_size = self.mmu_page_size();
        let number_of_pages = len.div_ceil(page_size);
        let mapped_len = number_of_pages * page_size;

        // Run unmapping in critical section
        //  It's important that while we operate on the MMU we have no interrupts, no FLASH access,
        //  no FLASH interrupts, no cache.
        critical_section::with(|_| {
            self.uncached_section(vaddr, mapped_len, || {
                let shift_code = cfg_match! {
                    any(feature = "esp32c5") => 16,
                    any(feature = "esp32c6", feature = "esp32c61") => ({
                        if page_size == 64 * 1_024 {
                            16
                        } else if page_size == 32 * 1_024 {
                            15
                        } else if page_size == 16 * 1_024 {
                            14
                        } else if page_size == 8 * 1_024 {
                            13
                        } else {
                            #[expect(clippy::unreachable, reason = "Hardware dictates it")]
                            {unreachable!("Hardware doesn't allow any other value")}
                        }
                    }),
                    _ => unimplemented!(),
                };

                let vaddr_mask = cfg_match! {
                    any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => (page_size * Self::MAX_PAGE_NUMBER) - 1,
                    _ => unimplemented!(),
                };

                for _ in 0..number_of_pages {
                    let entry_id = (vaddr & vaddr_mask) >> shift_code;
                    cfg_match! {
                        any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => ({
                            let spi0_regs = self.spi0.register_block();

                            // Write entry id to select
                            spi0_regs
                                .mmu_item_index()
                                .write(|w| {
                                    // safety: we are setting the correct hardware bits
                                    #[expect(clippy::cast_possible_truncation, reason = "Hardware dictates it")]
                                    unsafe {
                                        w.bits(entry_id as u32)
                                    }
                                });
                            // Write contents of the mapping
                            spi0_regs
                                .mmu_item_content().modify(|r, w| {
                                    // safety: we are setting the correct hardware bits
                                    unsafe {
                                        w.bits(r.bits() & !VALID_BIT)
                                    }
                            });
                        }),
                        _ => unimplemented!(),
                    }

                    vaddr += page_size;
                }
                Ok(())
            })
        })
    }
}

/// Base address of the Instruction Bus
pub const IBUS_BASE: usize = 0x4200_0000;
/// Base address of the Data Bus
pub const DBUS_BASE: usize = cfg_match! {
    any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => IBUS_BASE,
    _ => unimplemented!()
};

/// Valid bitflag of MMU page
const VALID_BIT: u32 = cfg_match! {
    feature = "esp32c6" => 1 << 9,
    any(feature = "esp32c5", feature = "esp32c61") => 1 << 10,
    _ => unimplemented!(),
};

/// Invalidates cache starting at `addr` for a `size` length
///
/// This is necessary when we map/unmap so that the CPU doesn't operate on stale cache which would
/// give us fake data.
///
/// # Safety
///
/// Makes use of ESP ROM functions
#[expect(
    clippy::cast_possible_truncation,
    reason = " We are running only on 32-bit targets"
)]
#[ram]
pub unsafe fn cache_invalidate_addr(addr: usize, size: usize) {
    if cfg!(feature = "esp32c5") || cfg!(feature = "esp32c6") || cfg!(feature = "esp32c61") {
        unsafe extern "C" {
            fn Cache_Invalidate_Addr(addr: u32, size: u32);
        }

        // safety: C FFI
        unsafe {
            Cache_Invalidate_Addr(addr as u32, size as u32);
        }
    } else {
        #[expect(
            clippy::unimplemented,
            reason = "This is the only hardware we support at this time"
        )]
        {
            unimplemented!("Only ESP32-C5, ESP32-C6 and ESP32-C61 are supported at the moment");
        }
    }
}

/// Stops the cache to allow MMU mapping amendments
///
/// # Safety
///
/// Makes use of ESP ROM functions
#[ram]
pub unsafe fn cache_stop() {
    // Freeze cache
    cfg_match! {
        // safety: Calls C FFI as dictated by ROM functions in esp-idf
        any(feature = "esp32c6") => unsafe {
            unsafe extern "C" {
                fn Cache_Freeze_ICache_Enable(mode: u32);
            }

            // safety: C FFI
            Cache_Freeze_ICache_Enable(0);
        },
        // safety: Calls C FFI as dictated by ROM functions in esp-idf
        any(feature = "esp32c5", feature = "esp32c61") => unsafe {
            unsafe extern "C" {
                fn Cache_Freeze_Enable(mode: u32);
            }

            // safety: C FFI
            Cache_Freeze_Enable(0);
        },
        _ => ({
            #[expect(
                clippy::unimplemented,
                reason = "This is the only hardware we support at this time"
            )]
            {
                unimplemented!("Only ESP32-C5, ESP32-C6 and ESP32-C61 are supported at the moment");
            }
        }),
    }
}

/// Starts the cache
///
/// # Safety
///
/// Makes use of ESP ROM functions
#[ram]
pub unsafe fn cache_start() {
    cfg_match! {
        // safety: Calls C FFI as dictated by ROM functions in esp-idf
        any(feature = "esp32c6") => unsafe {
            unsafe extern "C" {
                fn Cache_Freeze_ICache_Disable();
            }

            // safety: C FFI
            Cache_Freeze_ICache_Disable();
        },
        // safety: Calls C FFI as dictated by ROM functions in esp-idf
        any(feature = "esp32c5", feature = "esp32c61") => unsafe {
            unsafe extern "C" {
                fn Cache_Freeze_Disable();
            }

            // safety: C FFI
            Cache_Freeze_Disable();
        },
        _ => ({
            #[expect(
                clippy::unimplemented,
                reason = "This is the only hardware we support at this time"
            )]
            {
                unimplemented!("Only ESP32-C5, ESP32-C6 and ESP32-C61 are supported at the moment");
            }
        }),
    }
}

// Re-export so the macro can use
#[doc(hidden)]
pub mod __reexports {
    pub use cfg_match;
}
