//! Module for the structures required to deploy and run Wasm binaries using the Wasmtime runtime

use std::path::PathBuf;
use std::sync::Arc;

use sorg_common::custom_err;
use wasmtime::{Config, Engine, Linker};

use crate::{
    Result,
    wasm::{cell::state::CellState, host_functions::link_cell_functions},
};

pub(crate) mod cell;
mod host_functions;
pub(crate) mod module_load;

pub use host_functions::{
    CellIdentity, link_outlet_functions, link_tap_functions, release_sl_claim,
};

pub(crate) struct WasmEnvironment {
    pub(crate) engine: Engine,
    pub(crate) linker: Arc<Linker<CellState>>,
    pub(crate) runner_fuel: u64,
    pub(crate) fuel_yield_interval: u64,
}

impl WasmEnvironment {
    pub(super) fn new(
        runner_fuel: u64,
        fuel_yield_interval: u64,
        signal_layer_socket: Option<PathBuf>,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);

        let engine = Engine::new(&config)
            .map_err(|err| custom_err!("error creating wasmtime engine: {err}"))?;

        // D9: one shared TapClient per environment, captured in the linker closures.
        // S4/D3: resolve the tap socket path (explicit override, else
        // default_socket_path). If it cannot be determined — neither /run/peeriot
        // writable nor XDG_RUNTIME_DIR set — do NOT fail runtime setup: construct a
        // fail-closed client so cells that read taps see "unavailable" (D3) while
        // cells that never touch a tap still deploy and run. A missing tap socket
        // must not take down the whole cell host.
        let socket_path = signal_layer_socket.or_else(signal_layer_ipc::default_socket_path);
        let tap_client = Arc::new(match socket_path {
            Some(p) => signal_layer_ipc::TapClient::new(p),
            None => {
                tracing::warn!(
                    "signal-layer tap socket path unavailable \
                     (/run/peeriot not writable and XDG_RUNTIME_DIR not set); \
                     tap reads will report Unavailable until a socket is configured"
                );
                signal_layer_ipc::TapClient::unavailable()
            }
        });

        let linker_cells = linker_for_cells(&engine, tap_client)?;

        Ok(Self {
            engine,
            linker: Arc::new(linker_cells),
            runner_fuel,
            fuel_yield_interval,
        })
    }
}

fn linker_for_cells(
    engine: &Engine,
    tap_client: Arc<signal_layer_ipc::TapClient>,
) -> Result<Linker<CellState>> {
    let mut linker = Linker::new(engine);
    link_cell_functions(&mut linker)?;
    host_functions::link_tap_functions(&mut linker, Arc::clone(&tap_client))?;
    host_functions::link_outlet_functions(&mut linker, tap_client)?;
    Ok(linker)
}
