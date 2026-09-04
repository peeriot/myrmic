//! Main-stack high-water-mark measurement (debug aid, `stack-hwm` feature).
//!
//! The main embassy-executor runs on the linker stack (`_stack_start_cpu0` down
//! to `_stack_end_cpu0`), whose size is only whatever `.bss` and the heap leave
//! over. [`paint_unused_stack`] fills the unused stack with a sentinel at boot;
//! [`stack_high_water_mark`] later reports how deep the stack has been used.

unsafe extern "C" {
    /// Stack top — highest address; the stack grows down from here.
    static _stack_start_cpu0: u32;
    /// Stack bottom — lowest valid address (= end of `.bss`).
    static _stack_end_cpu0: u32;
    /// esp-hal's stack-guard word, watched by a debug trigger. The paint must
    /// stay above it: writing it would fire the guard.
    static __stack_chk_guard: u32;
}

const SENTINEL: u32 = 0xAAAA_AAAA;

/// Lowest address the paint/scan touches.
///
/// With the inverted heap budget (swarm#1344) the heap now occupies the lower
/// part of the old stack region, so the live main stack is only the top
/// `RESERVED_MAIN_STACK` bytes. Floor the scan at that reserve boundary — never
/// the guard word near the end of `.bss` — or the paint would stomp the heap.
fn scan_bottom() -> usize {
    let reserve_floor = ((&raw const _stack_start_cpu0) as usize)
        .saturating_sub(esp_common::esp_heap::RESERVED_MAIN_STACK);
    let guard = (&raw const __stack_chk_guard) as usize + 4;
    reserve_floor.max(guard)
}

/// Fill the currently-unused stack (below the caller's frame, down to the guard
/// word) with a sentinel. Call once, early, before the deep tasks run.
///
/// Don't inline it, otherwise, the probe's address could be placed mid-frame,
/// which means it starts stomping on variables declared after it.
#[inline(never)]
pub fn paint_unused_stack() {
    const MARGIN: usize = 512; // headroom below the probe for the live frame
    let bottom = scan_bottom();
    // A local's address approximates the current stack pointer.
    let probe = 0u8;
    let top = ((&raw const probe) as usize).saturating_sub(MARGIN) & !0b11;
    // Nothing to paint if the live frame leaves no room above the guard.
    if top <= bottom {
        return;
    }
    critical_section::with(|_| {
        let mut p = bottom;
        while p < top {
            // SAFETY: `[bottom, top)` is unused stack below the live frame, and
            // interrupts are masked so the SP can't descend into it mid-fill.
            unsafe { (p as *mut u32).write_volatile(SENTINEL) };
            p += 4;
        }
    });
}

/// Peak main-stack usage in bytes since [`paint_unused_stack`] — the deepest the
/// SP has reached, i.e. how much of the stack is no longer the sentinel.
///
/// Approximate: live data that happens to equal the sentinel reads as unused.
pub fn stack_high_water_mark() -> usize {
    let top = (&raw const _stack_start_cpu0) as usize;
    let mut p = scan_bottom();
    while p < top {
        // SAFETY: reads our own (painted) stack region word by word.
        if unsafe { (p as *const u32).read_volatile() } != SENTINEL {
            break;
        }
        p += 4;
    }
    // `p` is the deepest used address; usage is everything above it.
    // `saturating_sub`: an unexpected layout (scan start at/above the top) yields 0, not a wrapped value.
    top.saturating_sub(p)
}
