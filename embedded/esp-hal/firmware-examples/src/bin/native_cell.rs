//! Be the cell, natively — no WASM runtime at all.
//!
//! `register` claims this node's cell slot for the firmware. The identity is
//! folded from the SRN offline, so it is settled before the radio is up, and
//! `start` therefore knows not to bring up the WAMR thread, the module storage
//! or the flash MMU mapping. Another cell addressing `demo/thermostat` derives
//! the same SRI and can send to it.
//!
//! Because nothing is ever deployed here, `partitions.toml` can give the whole
//! flash to the firmware.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use esp_firmware::esp_hal::gpio::Flex;
use esp_firmware::{Board, Cell, Message, Network};

/// GPIO8 is the onboard LED on the ESP32-C6 devkit.
const LED: usize = 8;

#[esp_firmware::main]
async fn setup(board: &mut Board, net: Network, spawner: Spawner) {
    let led = board.take_pin(LED).expect("GPIO8 is free at boot");

    // Does not block: the swarm is told on the db service's next reconcile.
    let cell = net
        .register("demo/thermostat", &["set_target", "read"])
        .expect("the cell slot is free at boot");

    spawner.spawn(run(cell, led).unwrap());
}

#[embassy_executor::task]
async fn run(mut cell: Cell, mut led: Flex<'static>) {
    led.set_output_enable(true);

    // Sending works only once the swarm can see us; receiving simply stays
    // quiet until then, so waiting is only needed for the announcement.
    cell.online().await;
    log::info!("thermostat {} is online", cell.sri());
    let _ = cell.publish_event("started", b"".to_vec()).await;

    loop {
        match cell.recv().await {
            Message::Command {
                command, sender, ..
            } if command.as_ref() == "set_target" => {
                led.toggle();
                if let Some(sender) = sender {
                    let _ = cell.send_command(sender, "ack", b"ok".to_vec()).await;
                }
            }
            Message::Command { command, .. } => {
                log::info!("unhandled command {}", command.as_ref());
            }
            Message::Event { event, .. } => {
                log::info!("event {}", event.as_ref());
            }
        }
    }
}
