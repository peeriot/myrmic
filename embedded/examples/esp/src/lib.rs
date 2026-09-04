#![no_std]

pub mod tls_provider;
pub use tls_provider::CaCheckingProvider;

/// Allocate static storage in `no_std` targets and provide an `esp32` fallback.
#[macro_export]
macro_rules! mk_static {
    ($t:ty) => {{
        #[cfg(not(feature = "esp32"))]
        {
            static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
            STATIC_CELL.uninit()
        }
        #[cfg(feature = "esp32")]
        alloc::boxed::Box::leak(alloc::boxed::Box::<$t>::new_uninit())
    }};
    ($t:ty, $val:expr) => {{
        #[cfg(not(feature = "esp32"))]
        {
            static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
            #[deny(unused_attributes)]
            let x = STATIC_CELL.uninit().write($val);
            x
        }
        #[cfg(feature = "esp32")]
        alloc::boxed::Box::leak(alloc::boxed::Box::<$t>::new($val))
    }};
}
