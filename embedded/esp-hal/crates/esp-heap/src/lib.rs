//! Firmware heap setup with an inverted RAM budget (swarm#1344).
//!
//! esp-hal's `stack.x` makes the main stack the *leftover* of RWDATA: it spans
//! from the end of `.data`/`.bss` up to the top of RAM. The historical model
//! reserved a **fixed heap** static in `.bss` and let the stack take whatever
//! remained — so every byte of `.bss` growth (a `ble` build, a pipeline, more
//! code) silently shrank the stack until the linker's `Main stack is smaller
//! than N bytes` assert tripped. We paid that reactively three times
//! (#968, #1323, #1324).
//!
//! This crate inverts it: reserve a **fixed, feature-independent** main stack
//! ([`RESERVED_MAIN_STACK`]) and hand *all remaining* internal RAM to the heap,
//! computed at boot from the linker stack-region symbols. Now `.bss` growth
//! shrinks the heap (graceful — allocation pressure, and the heap is large with
//! the dram2 and PSRAM regions) instead of the stack (a hard fault). The only
//! knob is the reserve, which does not depend on the feature set.
//!
//! Safe because the measured peak main-stack use is ~2 KB — the deep zenoh/db
//! poll chains run on their own `NET_STACK`, so the main stack only carries the
//! shallow tasks. Verify the reserve with the firmware `stack-hwm` feature.
//!
//! The firmware calls [`setup_heap!`], which claims the computed main region,
//! reclaims the `dram2` segment, adds PSRAM on chips that have it, and logs the
//! boot memory summary (all regions + total, warning below [`HEAP_WARN_FLOOR`]).
//! On PSRAM chips the External region is registered first so general (untagged)
//! allocations prefer PSRAM, leaving the scarce internal DRAM for allocations
//! that request `MemoryCapability::Internal` (WiFi/BLE DMA buffers, RTOS task
//! stacks). See [`setup_heap!`] for the ordering rationale.

#![cfg_attr(not(test), no_std)]

/// Bytes reserved at the top of RWDATA for the main stack; the heap takes the
/// rest. Feature-independent: the value that makes the budget robust. Measured
/// peak main-stack use is ~2 KB (deep chains live on `NET_STACK`), so this has
/// wide margin — confirm with the `stack-hwm` feature after a real workload.
pub const RESERVED_MAIN_STACK: usize = 16 * 1024;

/// Below this computed main-heap size, [`setup_heap!`] logs a warning: the
/// firmware still boots, but headroom is getting thin and something (a feature,
/// code growth) is eating the budget. Sits well above the fatal floor.
pub const HEAP_WARN_FLOOR: usize = 96 * 1024;

/// Below this the computed main heap is treated as a fatal misconfiguration:
/// the reserve plus a workable heap no longer fit, so `.bss` has grown past
/// what the chip can carry. Replaces the compile-time stack assert as the
/// backstop now that the leftover no longer gates the stack.
pub const HEAP_MIN_FLOOR: usize = 16 * 1024;

/// Size of the reclaimed `dram2` segment added as internal heap.
pub const DRAM2_SIZE: usize = 64 * 1024;

#[cfg(target_os = "none")]
unsafe extern "C" {
    /// Top of the main stack region (high address); the stack grows down.
    static _stack_start_cpu0: u32;
    /// esp-hal's main-stack guard word, placed by `stack.x` at
    /// `_stack_end_cpu0 + ESP_HAL_CONFIG_STACK_GUARD_OFFSET` — the bottom of the
    /// stack region. When `stack_guard_monitoring` is on, esp-hal watchpoints
    /// this address, so the heap must begin *above* it, not at `_stack_end_cpu0`.
    static __stack_chk_guard: u32;
}

/// Outcome of computing the main-heap carve — reported by [`setup_heap!`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapCarve {
    /// A usable heap of `size` bytes at `start`.
    Ok { start: usize, size: usize },
    /// The region holds the reserve but the heap is below [`HEAP_MIN_FLOOR`].
    TooSmall { size: usize },
    /// The stack region cannot even hold [`RESERVED_MAIN_STACK`].
    NoRoom,
}

/// Pure computation of the heap carve: reserve `reserved_stack` at the top
/// (`stack_start`) and give the span from `heap_floor` up to that reserve to the
/// heap. `heap_floor` is the lowest usable heap address — above esp-hal's
/// main-stack guard word, never `_stack_end_cpu0` itself, or the allocator would
/// write the watched guard address (swarm#1344). No hardware access — unit-tested
/// on the host.
///
/// Returns [`HeapCarve::NoRoom`] if the region is smaller than the reserve,
/// [`HeapCarve::TooSmall`] if what is left is below [`HEAP_MIN_FLOOR`], else
/// [`HeapCarve::Ok`].
#[must_use]
pub fn compute_carve(heap_floor: usize, stack_start: usize, reserved_stack: usize) -> HeapCarve {
    let Some(region) = stack_start.checked_sub(heap_floor) else {
        return HeapCarve::NoRoom;
    };
    let Some(size) = region.checked_sub(reserved_stack) else {
        return HeapCarve::NoRoom;
    };
    if size < HEAP_MIN_FLOOR {
        return HeapCarve::TooSmall { size };
    }
    HeapCarve::Ok {
        start: heap_floor,
        size,
    }
}

/// Read the linker stack-region symbols and compute the main-heap carve.
///
/// The heap starts one word *above* esp-hal's main-stack guard word
/// (`__stack_chk_guard`, at the bottom of the stack region): esp-hal's
/// `stack_guard_monitoring` watchpoints that address, so a heap that began at
/// `_stack_end_cpu0` would hand the allocator the guard and the first write
/// there faults as a (false) stack-guard violation (swarm#1344).
///
/// # Safety
///
/// Reads `extern` linker symbols; the addresses are valid for the whole
/// program. Call once, early in `main`, before the deep tasks run.
#[cfg(target_os = "none")]
#[must_use]
pub unsafe fn main_heap_carve() -> HeapCarve {
    let heap_floor = (&raw const __stack_chk_guard as usize) + core::mem::size_of::<u32>();
    let stack_start = &raw const _stack_start_cpu0 as usize;
    compute_carve(heap_floor, stack_start, RESERVED_MAIN_STACK)
}

/// Add the computed main-heap region to the global allocator.
///
/// # Safety
///
/// Must run exactly once, before any deep task or allocation-heavy work. The
/// carve reserves [`RESERVED_MAIN_STACK`] for the live main stack; that reserve
/// must exceed the real peak stack use (verified with `stack-hwm`), or the
/// growing stack will descend into the heap.
#[cfg(target_os = "none")]
pub unsafe fn add_main_region(carve: HeapCarve) {
    if let HeapCarve::Ok { start, size } = carve {
        // SAFETY: `[start, start+size)` is the lower part of the linker stack
        // region — currently-unused RWDATA below the reserved live stack — and
        // this runs once, so the region is never registered twice.
        unsafe {
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
                start as *mut u8,
                size,
                esp_alloc::MemoryCapability::Internal.into(),
            ));
        }
    }
}

/// Reclaim the `dram2` segment (64 KB beyond the main RAM region) as internal
/// heap. Essential headroom: an active BLE connection plus GATT discovery
/// allocates ~50 KB, on top of the 128 KB WASM linear memory. Returns the size
/// added.
///
/// # Safety
///
/// Must run exactly once — the backing static is registered a single time.
#[cfg(target_os = "none")]
pub unsafe fn claim_dram2() -> usize {
    #[unsafe(link_section = ".dram2_uninit")]
    static mut HEAP2: core::mem::MaybeUninit<[u8; DRAM2_SIZE]> = core::mem::MaybeUninit::uninit();
    // SAFETY: HEAP2 is a dedicated static in the reserved dram2 segment, and
    // this runs once, so the region is never registered twice.
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            core::ptr::addr_of_mut!(HEAP2).cast::<u8>(),
            DRAM2_SIZE,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
    DRAM2_SIZE
}

/// Map PSRAM and add it as an `External` heap region (kept out of
/// `Internal`-capped DMA/controller allocations). [`setup_heap!`] registers it
/// before the internal regions so general (untagged) allocations prefer PSRAM and
/// leave internal DRAM for the DMA/stack allocations that require it. Returns the
/// mapped size in bytes.
///
/// # Safety
///
/// Must run exactly once with the real `PSRAM` peripheral.
#[cfg(any(feature = "esp32c5", feature = "esp32c61"))]
pub unsafe fn claim_psram(psram: esp_hal::peripherals::PSRAM<'static>) -> usize {
    let psram = esp_hal::psram::Psram::new(psram, esp_hal::psram::PsramConfig::default());
    let (start, size) = psram.raw_parts();
    // SAFETY: (start, size) describe the window esp-hal just mapped; runs once.
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            start,
            size,
            esp_alloc::MemoryCapability::External.into(),
        ));
    }
    // Keep the mapping alive for the program's lifetime.
    #[expect(
        clippy::forget_non_drop,
        reason = "keepalive for the program's duration"
    )]
    core::mem::forget(psram);
    size
}

/// Claim the internal heap: the computed main region plus the reclaimed dram2
/// segment. Returns the [`HeapCarve`] for the main region so the caller can log
/// and warn.
///
/// # Safety
///
/// Must run exactly once, early in `main`, before the deep tasks run.
#[cfg(target_os = "none")]
pub unsafe fn claim_internal() -> HeapCarve {
    // SAFETY: single call, before deep tasks — see the functions' contracts.
    let carve = unsafe { main_heap_carve() };
    // SAFETY: this is the single early call, so each region is added once.
    unsafe {
        add_main_region(carve);
        claim_dram2();
    }
    carve
}

/// Log the boot memory summary — every heap region and the internal total — and
/// warn/err against the floors. Called by [`setup_heap!`] after the regions are
/// claimed, so a field log always shows where the RAM went and how much headroom
/// is left (swarm#1344). `psram` is 0 on chips without it.
pub fn report_layout(carve: HeapCarve, psram: usize) {
    let main = match carve {
        HeapCarve::Ok { size, .. } | HeapCarve::TooSmall { size } => size,
        HeapCarve::NoRoom => 0,
    };
    log::info!(
        "[heap] boot layout: main {} KiB + dram2 {} KiB + PSRAM {} KiB = {} KiB total ({} KiB stack reserved)",
        main / 1024,
        DRAM2_SIZE / 1024,
        psram / 1024,
        (main + DRAM2_SIZE + psram) / 1024,
        RESERVED_MAIN_STACK / 1024,
    );
    match carve {
        HeapCarve::Ok { size, .. } if size < HEAP_WARN_FLOOR => log::warn!(
            "[heap] main region only {} KiB (warn floor {} KiB) — the RAM budget is getting thin",
            size / 1024,
            HEAP_WARN_FLOOR / 1024
        ),
        HeapCarve::TooSmall { size } => log::error!(
            "[heap] main region {} KiB is below the {} KiB floor — .data/.bss has outgrown the chip",
            size / 1024,
            HEAP_MIN_FLOOR / 1024
        ),
        HeapCarve::NoRoom => log::error!(
            "[heap] no room for the {}-KiB stack reserve",
            RESERVED_MAIN_STACK / 1024
        ),
        HeapCarve::Ok { .. } => {}
    }
}

/// Set up the firmware heap with the inverted RAM budget (swarm#1344):
///
/// 1. reserve [`RESERVED_MAIN_STACK`] for the main stack and add the rest of the
///    stack region as the internal heap,
/// 2. reclaim the dram2 segment,
/// 3. on chips with PSRAM, add PSRAM as the external overflow tier,
/// 4. log the boot memory summary (all regions + total), warning below
///    [`HEAP_WARN_FLOOR`].
///
/// Call once from the firmware entry point: `esp_heap::setup_heap!(peripherals);`.
/// References only `$crate::` items and the passed peripherals, so no imports
/// are required at the call site.
#[macro_export]
macro_rules! setup_heap {
    ($periphs:ident) => {{
        // On PSRAM chips, register PSRAM (External) BEFORE the internal regions so
        // general Rust allocations land in PSRAM first and leave internal DRAM for
        // the allocations that require it.
        #[cfg(any(feature = "esp32c5", feature = "esp32c61"))]
        // SAFETY: the PSRAM peripheral is taken by value and mapped once.
        let __psram = unsafe { $crate::claim_psram($periphs.PSRAM) };
        #[cfg(not(any(feature = "esp32c5", feature = "esp32c61")))]
        let __psram = 0usize;
        // SAFETY: `main` is the single `#[esp_rtos::main]` entry point, so each
        // region is claimed exactly once, before the deep tasks run.
        let __carve = unsafe { $crate::claim_internal() };
        $crate::report_layout(__carve, __psram);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carve_reserves_stack_and_gives_the_rest() {
        // 200 KB region, reserve 32 KB → 168 KB heap at the bottom.
        let carve = compute_carve(0x1000, 0x1000 + 200 * 1024, 32 * 1024);
        assert_eq!(
            carve,
            HeapCarve::Ok {
                start: 0x1000,
                size: 168 * 1024
            }
        );
    }

    #[test]
    fn carve_flags_a_heap_below_the_min_floor() {
        // 40 KB region, reserve 32 KB → 8 KB heap, under HEAP_MIN_FLOOR (16 KB).
        let carve = compute_carve(0, 40 * 1024, 32 * 1024);
        assert_eq!(carve, HeapCarve::TooSmall { size: 8 * 1024 });
    }

    #[test]
    fn carve_flags_no_room_for_the_reserve() {
        let carve = compute_carve(0, 16 * 1024, 32 * 1024);
        assert_eq!(carve, HeapCarve::NoRoom);
    }

    #[test]
    fn warn_floor_is_above_min_floor() {
        assert!(HEAP_WARN_FLOOR > HEAP_MIN_FLOOR);
    }
}
