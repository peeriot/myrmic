//! What the firmware is allowed to touch.
//!
//! [`Board`] holds the peripherals the shipped subsystems need. Taking one out
//! is how you turn its subsystem off: [`start`](crate::start) brings up only
//! what is still here when it runs, so "bring your own BLE stack" and "don't
//! run BLE at all" are the same gesture, and the borrow checker guarantees we
//! are not still driving hardware you took.

use esp_common::esp_mmu::Mmu;
use esp_hal::gpio::Flex;
use esp_hal::peripherals::{BT, FLASH, RTC_TIMER, TIMG1, WIFI};
use serde::Serialize;
use serde::de::DeserializeOwned;
use signal_layer_core::{OutletRegistry, TapRegistry, WireType};
use wasm_runtime::Pins;
use wasm_storage::PartitionLayout;

use crate::{Config, DeclareError, EventTap, Outlet, Tap};

/// The hardware and settings [`start`](crate::start) brings up.
///
/// Build one with the [`board!`](macro@crate::board) macro, adjust it in your setup
/// function, then hand it to [`start`](crate::start).
#[derive(Debug)]
pub struct Board {
    /// Knobs that are numbers rather than hardware — stack sizes, priorities.
    pub config: Config,
    pins: Pins,
    wifi: Option<WIFI<'static>>,
    bt: Option<BT<'static>>,
    flash: Option<FLASH<'static>>,
    mmu: Option<Mmu>,
    watchdog: Option<(TIMG1<'static>, RTC_TIMER<'static>)>,
    partition_layout: PartitionLayout,
    taps: TapRegistry,
    outlets: OutletRegistry,
}

impl Board {
    /// Assembles a board from its parts. Prefer the [`board!`](macro@crate::board)
    /// macro, which claims each part out of `Peripherals` for you.
    #[must_use]
    pub fn new(
        pins: Pins,
        wifi: WIFI<'static>,
        bt: BT<'static>,
        flash: FLASH<'static>,
        mmu: Mmu,
        watchdog: (TIMG1<'static>, RTC_TIMER<'static>),
        partition_layout: PartitionLayout,
    ) -> Self {
        Self {
            config: Config::default(),
            pins,
            wifi: Some(wifi),
            bt: Some(bt),
            flash: Some(flash),
            mmu: Some(mmu),
            watchdog: Some(watchdog),
            partition_layout,
            taps: TapRegistry::new(),
            outlets: OutletRegistry::new(),
        }
    }

    /// Declares a retained tap: a value this firmware publishes and a cell on
    /// this node reads by `name`. Holds the latest value only.
    ///
    /// `T` crosses to the cell as postcard and the cell checks its type id, so
    /// it must be a [`WireType`] the cell also knows: a primitive, or one of
    /// [`signal_layer_types`].
    ///
    /// # Errors
    ///
    /// [`DeclareError::Full`] past [`MAX_TAPS`](signal_layer_core::MAX_TAPS)
    /// taps, [`DeclareError::Duplicate`] if a tap of this name exists.
    pub fn tap<T>(&mut self, name: &'static str) -> Result<Tap<T>, DeclareError>
    where
        T: Serialize + WireType + Clone + Send + Sync + 'static,
    {
        Tap::declare(&mut self.taps, name)
    }

    /// Declares an event tap: a queue of discrete events this firmware emits
    /// and a cell drains by `name`. Shares the tap registry and its budget
    /// with [`tap`](Self::tap).
    ///
    /// # Errors
    ///
    /// As [`tap`](Self::tap).
    pub fn event_tap<T>(&mut self, name: &'static str) -> Result<EventTap<T>, DeclareError>
    where
        T: Serialize + WireType + Clone + Send + Sync + 'static,
    {
        EventTap::declare(&mut self.taps, name)
    }

    /// Declares an outlet: a command a cell on this node writes by `name` and
    /// this firmware acts on. Last write wins.
    ///
    /// Outlets have their own registry and namespace, so an outlet and a tap
    /// may share a name.
    ///
    /// # Errors
    ///
    /// [`DeclareError::Full`] past
    /// [`MAX_OUTLETS`](signal_layer_core::MAX_OUTLETS) outlets,
    /// [`DeclareError::Duplicate`] if an outlet of this name exists.
    pub fn outlet<T>(&mut self, name: &'static str) -> Result<Outlet<T>, DeclareError>
    where
        T: DeserializeOwned + WireType + Clone + Send + Sync + 'static,
    {
        Outlet::declare(&mut self.outlets, name)
    }

    /// The tap registry itself, for code that registers slots it already owns:
    /// a generated pipeline's `register_taps`. Prefer [`tap`](Self::tap) and
    /// [`event_tap`](Self::event_tap) for your own.
    pub fn taps(&mut self) -> &mut TapRegistry {
        &mut self.taps
    }

    /// The outlet registry itself, for a generated pipeline's
    /// `register_outlets`. Prefer [`outlet`](Self::outlet) for your own.
    pub fn outlets(&mut self) -> &mut OutletRegistry {
        &mut self.outlets
    }

    /// Takes GPIO `n` for your own use. The cell can no longer reach it —
    /// [`Pins`] already models a pin as absent, so the WASM side reports it
    /// unavailable rather than misbehaving.
    ///
    /// Returns `None` if the pin is not exposed on this chip, or if something
    /// already took it.
    pub fn take_pin(&mut self, n: usize) -> Option<Flex<'static>> {
        self.pins.get_mut(n)?.take()
    }

    /// Whether GPIO `n` is still available to the cell.
    #[must_use]
    pub fn has_pin(&self, n: usize) -> bool {
        self.pins.get(n).is_some_and(Option::is_some)
    }

    /// Takes the WiFi peripheral, so the shipped network service does not
    /// start.
    ///
    /// Without it this node joins no swarm: there is no zenoh session, no db,
    /// and no mailbox, so a cell — native or WASM — has nothing to talk to.
    /// Take it only if you are replacing the transport wholesale.
    pub fn take_wifi(&mut self) -> Option<WIFI<'static>> {
        self.wifi.take()
    }

    /// Whether the shipped network service will start.
    #[must_use]
    pub fn has_wifi(&self) -> bool {
        self.wifi.is_some()
    }

    /// Takes the Bluetooth peripheral, so the shipped NimBLE host and its HCI
    /// transport do not start and you can run your own stack on it.
    ///
    /// The cell-facing BLE host functions go with it: they are implemented
    /// against the shipped stack, so a cell on this node loses BLE unless you
    /// reimplement them. Without the `ble` feature nothing is listening for it
    /// anyway, and this is simply how you get at the peripheral.
    pub fn take_ble(&mut self) -> Option<BT<'static>> {
        self.bt.take()
    }

    /// Whether the shipped BLE stack will start. Always `false` without the
    /// `ble` feature, which compiles the stack out entirely.
    #[must_use]
    pub fn has_ble(&self) -> bool {
        cfg!(feature = "ble") && self.bt.is_some()
    }

    /// Takes the flash peripheral and the MMU, so the WASM host does not start.
    ///
    /// Nothing can then be deployed to this node as a module. A node whose
    /// firmware registers a native cell has already given up the cell slot, so
    /// dropping the WASM host too is what makes that build small — see
    /// [`Network::register`](crate::Network::register).
    pub fn take_wasm_host(&mut self) -> Option<(FLASH<'static>, Mmu)> {
        Some((self.flash.take()?, self.mmu.take()?))
    }

    /// Whether the WASM host will start.
    #[must_use]
    pub fn has_wasm_host(&self) -> bool {
        self.flash.is_some() && self.mmu.is_some()
    }

    /// Takes the watchdog timers, leaving the node unsupervised: no staged
    /// MWDT, no RWDT backstop, and no required-task heartbeat, so a wedged
    /// runtime hangs instead of resetting.
    ///
    /// There is no way to keep the watchdog and change how it decides — the
    /// required-task set and the timeouts are the firmware's, not a knob.
    pub fn take_watchdog(&mut self) -> Option<(TIMG1<'static>, RTC_TIMER<'static>)> {
        self.watchdog.take()
    }

    /// Whether the hardware watchdogs will be armed.
    #[must_use]
    pub fn has_watchdog(&self) -> bool {
        self.watchdog.is_some()
    }

    /// The AOT flash layout the WASM host will use.
    #[must_use]
    pub fn partition_layout(&self) -> PartitionLayout {
        self.partition_layout
    }

    /// Splits the board into the parts [`start`](crate::start) consumes.
    pub(crate) fn into_parts(self) -> BoardParts {
        BoardParts {
            config: self.config,
            pins: self.pins,
            wifi: self.wifi,
            bt: self.bt,
            wasm_host: self.flash.zip(self.mmu),
            watchdog: self.watchdog,
            partition_layout: self.partition_layout,
            taps: self.taps,
            outlets: self.outlets,
        }
    }
}

pub(crate) struct BoardParts {
    pub config: Config,
    pub pins: Pins,
    pub wifi: Option<WIFI<'static>>,
    pub bt: Option<BT<'static>>,
    pub wasm_host: Option<(FLASH<'static>, Mmu)>,
    pub watchdog: Option<(TIMG1<'static>, RTC_TIMER<'static>)>,
    pub partition_layout: PartitionLayout,
    pub taps: TapRegistry,
    pub outlets: OutletRegistry,
}

/// The AOT flash layout generated by `esp_firmware_build::configure()` in your
/// build script.
///
/// Expands to an `include!` of the generated file, so it reads *your* crate's
/// `OUT_DIR`. [`board!`](macro@crate::board) uses it for you; call it directly only
/// if you are building a [`Board`] by hand.
#[macro_export]
macro_rules! partition_layout {
    () => {
        include!(concat!(
            env!("OUT_DIR"),
            "/esp_firmware_partition_layout.rs"
        ))
    };
}

/// Pulls in the Signal Layer pipeline generated by
/// `esp_firmware_build::pipeline().…​.generate()` in your build script.
///
/// Expands to a `pipeline_config` module that `include!`s the generated file
/// from *your* crate's `OUT_DIR`, gated on the `pipeline` feature so the crate
/// still builds without it. The module keeps the generated `use`s and items out
/// of your `main.rs`, while the codegen-emitted macros (`pipeline_pins!`,
/// `pipeline_board_peripherals!`) and the `register_taps` / `register_outlets` /
/// `spawn_sources` entry points stay reachable at the crate root.
///
/// ```ignore
/// esp_firmware::pipeline!();
/// ```
#[macro_export]
macro_rules! pipeline {
    () => {
        #[cfg(feature = "pipeline")]
        #[rustfmt::skip]
        mod pipeline_config {
            #![allow(unused_imports, dead_code, unused_variables)]
            // Route the generated bare crate paths (`esp_hal::`, `embassy_sync::`,
            // `signal_layer_core::`, ...) through esp-firmware's re-exports, so a
            // firmware crate needs no direct dependency on the infrastructure
            // crates and cannot end up with a second version of the esp-hal fork.
            use $crate::{
                embassy_embedded_hal, embassy_sync, embassy_time, esp_hal, log,
                signal_layer_core, signal_layer_types, static_cell, wasm_runtime,
            };
            include!(concat!(env!("OUT_DIR"), "/pipeline.rs"));
        }
    };
}

/// Claims the firmware's peripherals out of `Peripherals`, yielding a
/// [`Board`].
///
/// A macro rather than a function because claiming moves individual fields out
/// of `Peripherals`; that only works inline, where the borrow checker can see
/// which fields went where. It is also what lets you claim your own hardware
/// first — everything this macro does not name stays yours:
///
/// ```ignore
/// let i2c = peripherals.I2C0;                    // yours
/// let board = esp_firmware::board!(peripherals); // the rest is the firmware's
/// ```
///
/// By default every unreserved GPIO goes to the cell. Pass your own [`Pins`] to
/// override that — the form a Signal Layer pipeline needs, since it claims pins
/// from the board manifest first:
///
/// ```ignore
/// let pins = pipeline_pins!(peripherals);
/// let board = esp_firmware::board!(peripherals, pins = pins);
/// ```
#[macro_export]
macro_rules! board {
    ($periph:ident) => {{
        // `pins_from_peripherals!` names `Pins` bare, and reads the chip
        // feature from the crate that invokes it — so a firmware crate must
        // define `esp32c5`/`esp32c6`/`esp32c61` itself and forward it here.
        use $crate::__reexports::wasm_runtime::Pins;
        let __pins = $crate::__reexports::wasm_runtime::pins_from_peripherals!($periph);
        $crate::board!($periph, pins = __pins)
    }};
    ($periph:ident, pins = $pins:expr) => {
        $crate::Board::new(
            $pins,
            $periph.WIFI,
            $periph.BT,
            $periph.FLASH,
            $crate::__reexports::esp_common::esp_mmu::mmu_from_peripherals!($periph),
            ($periph.TIMG1, $periph.RTC_TIMER),
            $crate::partition_layout!(),
        )
    };
}
