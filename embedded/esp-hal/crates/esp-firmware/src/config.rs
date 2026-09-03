//! Sizes and priorities. Hardware ownership decides *what* runs (see
//! [`Board`](crate::Board)); this decides *how*.

use core::time::Duration;

/// Thread stacks and scheduling priorities for the firmware's threads.
///
/// The defaults are the values the shipped firmware is validated at. They are
/// exposed because a board that carries extra work on these threads may need
/// more room — not because they are routine tuning. Shrinking a stack below
/// its default is how you get a silent overflow: the RTOS checks stacks only
/// at context switches, so the corruption surfaces somewhere else entirely.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Config {
    /// Stack for the network-service thread, which runs WiFi, the zenoh
    /// session and the db client.
    ///
    /// The zenoh/db poll chains are the deepest in the firmware, which is why
    /// they get an owned stack instead of a claim on whatever RAM `.bss`
    /// leaves the main stack. At 28 KB the cell-start chain overflowed this by
    /// ~768 B and silently overwrote the statics below it (#1323). There is no
    /// high-water-mark instrumentation for this thread — `stack-hwm` paints
    /// only the main linker stack — so this figure rests on the HIL suite
    /// passing, not on a measured peak. Do not shrink it without adding that
    /// measurement first.
    pub net_stack: usize,

    /// Stack for the WAMR thread that executes the cell.
    pub wasm_stack: usize,

    /// Stack for the BLE HCI transport thread. Profiled floor is ~1.5 KB.
    pub ble_hci_stack: usize,

    /// Stack for the NimBLE host thread. Profiled floor is ~1.5 KB.
    pub ble_host_stack: usize,

    /// Priority of the BLE HCI transport thread.
    ///
    /// Must outrank everything else: if this thread is kept off the CPU,
    /// controller-to-host HCI packets are not drained and their buffers leak.
    pub ble_hci_priority: u32,

    /// Priority of the NimBLE host thread, below the transport and above the
    /// controller (which esp-radio spawns at 29).
    pub ble_host_priority: u32,

    /// Priority of the network-service thread. Shares the default priority
    /// with the main executor, so the two time-slice.
    pub net_priority: u32,

    /// Priority of the WAMR thread. Lowest, so embassy can preempt the cell.
    pub wasm_priority: u32,

    /// The silence this node asks observers to tolerate: three renewal periods,
    /// so a couple of dropped radio rounds never declare it dead.
    pub node_lease_ttl: Duration,

    /// Liveness-lease renewal period, slower than the Linux exec's 10s to
    /// respect the radio budget; [`Self::node_lease_ttl`] absorbs the sparser cadence.
    pub node_lease_renewal_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            net_stack: 40 * 1024,
            wasm_stack: 25 * 1024,
            ble_hci_stack: 4 * 1024,
            ble_host_stack: 4 * 1024,
            ble_hci_priority: 40,
            ble_host_priority: 30,
            net_priority: 1,
            wasm_priority: 0,
            node_lease_ttl: Duration::from_mins(1),
            node_lease_renewal_interval: Duration::from_secs(20),
        }
    }
}
