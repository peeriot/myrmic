//! Random number generator source.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

pub use rand_core::RngCore;

static RANDOM: Mutex<CriticalSectionRawMutex, RefCell<Option<RandomRef<'static>>>> =
    Mutex::new(RefCell::new(None));

/// A random number generator source.
///
/// NOTE: This type should NOT be used with `core::mem::forget` as it expects itts drop fn to run
/// so that the internal implementation of a custom `getrandom` provider that this type provides
/// is properly removed on drop.
pub struct RandomSource<'d> {
    _dummy: &'d (),
}

impl<'d> RandomSource<'d> {
    /// Create a new `RandomSource` from the given RNG.
    ///
    /// # Arguments
    /// - `rng`: A mutable reference to a random number generator that implements `RngCore + Send`.
    pub fn new(rng: &'d mut (dyn RngCore + Send)) -> Self {
        RANDOM.lock(|rref| {
            let mut rref = rref.borrow_mut();
            if rref.is_some() {
                panic!("RandomSource already initialized");
            }

            *rref = Some(RandomRef {
                rng: unsafe {
                    core::mem::transmute::<
                        &'d mut (dyn RngCore + Send),
                        &'static mut (dyn RngCore + Send),
                    >(rng)
                },
                ref_count: 1,
            });
        });

        Self { _dummy: &() }
    }

    /// Fill the given buffer with random bytes.
    ///
    /// # Arguments
    /// - `buf`: The buffer to fill with random bytes.
    pub fn fill_bytes(&self, buf: &mut [u8]) {
        Self::static_fill_bytes(buf);
    }

    fn static_fill_bytes(buf: &mut [u8]) {
        RANDOM.lock(|rref| {
            if let Some(rref) = rref.borrow_mut().as_mut() {
                rref.rng.fill_bytes(buf);
            } else {
                panic!("RandomSource not initialized");
            }
        });
    }
}

impl Clone for RandomSource<'_> {
    fn clone(&self) -> Self {
        RANDOM.lock(|rref| {
            let mut rref = rref.borrow_mut();

            let mut r = unwrap!(rref.take());

            r.ref_count += 1;
            *rref = Some(r);
        });

        Self { _dummy: &() }
    }
}

impl Drop for RandomSource<'_> {
    fn drop(&mut self) {
        RANDOM.lock(|rref| {
            let mut rref = rref.borrow_mut();

            let mut r = unwrap!(rref.take());

            r.ref_count -= 1;
            if r.ref_count > 0 {
                *rref = Some(r);
            }
        });
    }
}

struct RandomRef<'a> {
    rng: &'a mut (dyn RngCore + Send),
    ref_count: u32,
}

//#[cfg(feature = "getrandom-custom")]
#[allow(unsafe_op_in_unsafe_fn)]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    let dest = core::slice::from_raw_parts_mut(dest, len);
    RandomSource::<'static>::static_fill_bytes(dest);

    Ok(())
}
