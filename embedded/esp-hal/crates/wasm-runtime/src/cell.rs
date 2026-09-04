#[cfg(feature = "ble")]
use alloc::string::String;
use alloc::vec::Vec;

use db_client::v1::models::Id;
use myrmic_common::cells::{Command, Event, Sri};

use crate::async_request::cell_host::CommandHandledGuard;
pub(crate) use crate::async_request::timers::TimerCompletion;

/// Where a command came from, and so what consuming it means.
#[derive(Debug)]
pub enum CommandOrigin {
    /// Read from the cell's mailbox and still in it. The runtime removes `msg_id`
    /// inside the handler's transaction once the handler succeeds, so a failed
    /// handler leaves the command queued for another delivery. Dropping `handled`
    /// releases the poller either way.
    Mailbox {
        msg_id: Id,
        handled: CommandHandledGuard,
    },
    /// Raised by the runtime for the cell itself — a BLE result landing on the
    /// callback it registered. Nothing to consume, nobody waiting.
    Local,
}

impl CommandOrigin {
    /// The mailbox message this call came from, if it came from one.
    pub(crate) fn mailbox_entry(&self) -> Option<&Id> {
        match self {
            CommandOrigin::Mailbox { msg_id, .. } => Some(msg_id),
            CommandOrigin::Local => None,
        }
    }
}

/// A message for the Cell to process
#[derive(Debug)]
pub enum CellMessage {
    /// Cell Command
    Command {
        command: Command,
        payload: Option<Vec<u8>>,
        sender: Option<Sri>,
        origin: CommandOrigin,
    },
    /// Cell Event
    Event {
        event: Event,
        payload: Vec<u8>,
        sender: Option<Sri>,
    },
    /// Timer tick — call the named exported function
    TimerTick {
        export_name: heapless::String<64>,
        completed: Option<TimerCompletion>,
    },
    /// BLE callback — a call to the `command_<export_name>` handler the cell
    /// named as a callback, with `payload` as its argument. Enqueued by the BLE
    /// manager task, which cannot resolve the cell's SRI itself; the runtime
    /// turns it into an ordinary command call on delivery.
    #[cfg(feature = "ble")]
    BleCallback {
        export_name: String,
        payload: Vec<u8>,
    },
    /// Request for the Cell to be destroyed
    Destroy,
}

impl CellMessage {
    /// The mailbox message this call came from, if it came from one. Lets the
    /// poller tell a re-read of an unconsumed command from a freshly arrived one.
    #[must_use]
    pub fn mailbox_entry(&self) -> Option<&Id> {
        match self {
            CellMessage::Command { origin, .. } => origin.mailbox_entry(),
            _ => None,
        }
    }
}
