// Change this ONE line to swap allocator globally for all users of the macro.
// going with LLffHeap for now, since smaller code size
// we expect simpler modules at this point and are not too concerned about fragmentation
#[doc(hidden)]
pub type __DefaultHeap = super::__reexports::embedded_alloc::LlffHeap;

/// Emits the cell's global allocator: an `embedded-alloc` heap of `$heap_size`
/// bytes, the `init_allocator` export the host calls before running the module,
/// and a no-op `critical-section` backend (Wasm cells are single-threaded).
///
/// Invoked for you by [`cell_prelude!`](crate::cell_prelude), which also sizes
/// the heap; call it directly only when opting out of the prelude.
#[macro_export]
macro_rules! define_alloc_heap {
    ($heap_size:expr) => {
        const _: () = {
            // Add code to setup global allocator (using embedded-alloc::Heap)
            #[global_allocator]
            static HEAP: $crate::__DefaultHeap = <$crate::__DefaultHeap>::empty();

            #[unsafe(no_mangle)]
            pub extern "C" fn init_allocator() {
                static mut HEAP_MEM: [core::mem::MaybeUninit<u8>; { $heap_size }] =
                    [core::mem::MaybeUninit::uninit(); { $heap_size }];

                // SAFETY: we hand a stable, page-aligned region we own for the life of the program.
                unsafe { HEAP.init(core::ptr::addr_of_mut!(HEAP_MEM) as usize, $heap_size) }
            }

            // Add code to provide a default impl for the critical section
            // We need some implementation, since embedded alloc pulls in critical section and we don't
            // want the module to require extra acquire/release imports.
            // We are in single-threaded Wasm, so we are okay with a Noop here.
            mod __wasm_cs_backend {
                use $crate::__reexports::critical_section::RawRestoreState;

                pub struct WasmCs;
                // SAFETY: Wasm cells are single-threaded with no interrupts, so
                // a no-op critical section cannot be preempted.
                unsafe impl $crate::__reexports::critical_section::Impl for WasmCs {
                    #[inline]
                    unsafe fn acquire() -> RawRestoreState {}
                    #[inline]
                    unsafe fn release(_: RawRestoreState) {}
                }
                $crate::__reexports::critical_section::set_impl!(WasmCs);
            }
        };
    };
}
