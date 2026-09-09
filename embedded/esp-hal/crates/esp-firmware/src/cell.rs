//! Being a cell, natively.
//!
//! A node hosts exactly one cell. Normally the orchestrator fills that slot by
//! deploying a WASM module; [`Network::register`] fills it from the firmware
//! instead, so the node *is* the cell and no WASM runtime is needed.
//!
//! # Identity is offline
//!
//! An SRI is a `UUIDv5` folded over the `/`-separated segments of an SRN, so
//! `register("myapp/thermostat")` resolves to its identity with no radio, no
//! session and no db — and any other cell addressing `myapp/thermostat`
//! derives the identical UUID, equally offline. Registration therefore cannot
//! fail for want of a network, and [`start`](crate::start) can decide not to
//! bring up the WASM host before the radio has ever come up.
//!
//! What *does* need the network is being *reachable*: the placement row that
//! tells the swarm this node holds the cell. That is written by the db service
//! on its own reconcile timer and re-asserted after a reconnect, exactly as
//! this node's exec registration and lease renewal already are. So
//! [`register`](Network::register) does not block, and [`Cell::online`] is how
//! you wait for the row to land if you care.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use cell_protocol::{CellAttachment, MailboxCommand, MailboxEvent};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_sync::signal::Signal;
use myrmic_common::cells::{Command, Event, Sri};
use wasm_runtime::CellMessage;
use wasm_runtime::async_request::{CommandHandledGuard, DbClientRequest, DbClientResponse};

/// Set once a native registration has claimed this node's cell slot.
static SLOT_CLAIMED: AtomicBool = AtomicBool::new(false);

/// The registration the db service seeds its cell slot from, and re-asserts
/// after a reconnect. Written once during setup, read by the network service
/// thread when it starts.
static REGISTRATION: critical_section::Mutex<RefCell<Option<Registration>>> =
    critical_section::Mutex::new(RefCell::new(None));

/// Raised by the db service once the placement row for the native cell is
/// committed and its mailbox subscription is live.
static ONLINE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// A native cell's declared identity and command surface.
#[derive(Debug, Clone)]
pub struct Registration {
    /// The cell's identity, derived from its SRN.
    pub sri: Sri,
    /// The commands this cell answers, as advertised to the swarm.
    pub commands: Vec<Command>,
    /// The SRN it was declared under, kept for display.
    pub name: String,
}

/// Whether this node's cell slot is held by native firmware.
pub(crate) fn slot_is_claimed() -> bool {
    SLOT_CLAIMED.load(Ordering::Acquire)
}

/// The native registration, if there is one.
///
/// Read by the network service when it starts, so a service restart re-asserts
/// the same claim rather than losing it.
#[must_use]
pub fn registration() -> Option<Registration> {
    critical_section::with(|cs| REGISTRATION.borrow_ref(cs).clone())
}

/// Marks the native cell as reachable. Called by the db service once its
/// placement row is committed.
pub fn mark_online() {
    ONLINE.signal(());
}

/// Why a registration could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The SRN is not a valid cell path.
    BadName,
    /// One of the advertised command names is not a valid identifier.
    BadCommand,
    /// This node's cell slot is already claimed — a node hosts one cell.
    SlotTaken,
}

impl core::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadName => f.write_str("not a valid SRN path"),
            Self::BadCommand => f.write_str("not a valid command name"),
            Self::SlotTaken => f.write_str("this node's cell slot is already claimed"),
        }
    }
}

/// This node's handle on the swarm.
///
/// Obtained from [`network()`](crate::network) — or handed to your setup
/// function by [`main`](macro@crate::main) — and usable from boot: it addresses
/// channels the network service drains once it comes up.
#[derive(Debug, Clone, Copy)]
pub struct Network {
    requests: Sender<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    responses: Receiver<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
}

impl Network {
    pub(crate) fn new(
        requests: Sender<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
        responses: Receiver<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    ) -> Self {
        Self {
            requests,
            responses,
        }
    }

    /// Claims this node's cell slot for native firmware, under the identity
    /// `srn` names and advertising `commands`.
    ///
    /// Returns immediately — the identity is derived locally. The swarm learns
    /// of it when the db service next reconciles.
    ///
    /// Claiming the slot means [`start`](crate::start) will not bring up the
    /// WASM host, so nothing can be deployed to this node as a module. That is
    /// where the flash and RAM savings come from, and it is why a second call
    /// fails rather than sharing.
    ///
    /// # Errors
    ///
    /// [`RegisterError::BadName`] if `srn` is not a valid cell path,
    /// [`RegisterError::BadCommand`] if a command name is not a valid
    /// identifier, and [`RegisterError::SlotTaken`] if the slot is already
    /// claimed.
    pub fn register(self, srn: &str, commands: &[&str]) -> Result<Cell, RegisterError> {
        let sri = Sri::from_target(srn).map_err(|_| RegisterError::BadName)?;
        let commands: Vec<Command> = commands
            .iter()
            .map(|c| Command::try_from(*c).map_err(|_| RegisterError::BadCommand))
            .collect::<Result<_, _>>()?;

        if SLOT_CLAIMED.swap(true, Ordering::AcqRel) {
            return Err(RegisterError::SlotTaken);
        }

        critical_section::with(|cs| {
            REGISTRATION.borrow_ref_mut(cs).replace(Registration {
                sri,
                commands,
                name: String::from(srn),
            });
        });
        log::info!("[esp-firmware] cell slot claimed natively: {srn} ({sri})");

        Ok(Cell {
            sri,
            net: self,
            handling: false,
        })
    }
}

/// This node, as an addressable cell.
///
/// Holds the db channel pair exclusively: claiming the slot natively stops the
/// WASM host from starting, so nothing else is issuing requests on it. Keep the
/// one `Cell` you were given rather than sharing it between tasks — the
/// request/response pair carries no correlation ids, so concurrent callers
/// would mispair each other's replies.
#[derive(Debug)]
pub struct Cell {
    sri: Sri,
    net: Network,
    /// Whether a mailbox command is still ours to finish. The db service holds
    /// the next one back until we say we are done, and only removes the
    /// current one then — so delivery survives a reset mid-handling.
    handling: bool,
}

impl Cell {
    /// This cell's identity. Available from the moment it is registered.
    #[must_use]
    pub fn sri(&self) -> Sri {
        self.sri
    }

    /// Waits until the swarm can address this cell — its placement row is
    /// committed and its mailbox subscription is live. Returns immediately if
    /// that already happened.
    pub async fn online(&self) {
        if !ONLINE.signaled() {
            ONLINE.wait().await;
        }
    }

    /// Receives the next command or event addressed to this cell.
    ///
    /// Blocks until one arrives; nothing does before the node is
    /// [`online`](Self::online).
    ///
    /// Asking for the next message is how you acknowledge the last one: only
    /// then is the previous command removed from the mailbox, and only then is
    /// the one behind it delivered. A reset before that redelivers it, so a
    /// command is handled at least once. Commands are therefore handled one at
    /// a time, in order — do the work before looping, not in a spawned task.
    pub async fn recv(&mut self) -> Message {
        if self.handling {
            self.handling = false;
            // Dropping the guard releases the db service to remove the command
            // we were given and offer the next.
            drop(CommandHandledGuard);
        }
        loop {
            match wasm_runtime::async_request::CELL_MSG_CHANNEL
                .receive()
                .await
            {
                CellMessage::Command {
                    command,
                    payload,
                    sender,
                    ..
                } => {
                    self.handling = true;
                    return Message::Command {
                        command,
                        payload: payload.unwrap_or_default(),
                        sender,
                    };
                }
                CellMessage::Event {
                    event,
                    payload,
                    sender,
                } => {
                    return Message::Event {
                        event,
                        payload,
                        sender,
                    };
                }
                // Timer ticks, BLE callbacks and teardown are WASM-host
                // concerns: a native cell drives its own timers and owns its
                // own BLE stack, and there is no module to destroy.
                _ => continue,
            }
        }
    }

    /// Sends a fire-and-forget command to another cell, stamped as from this
    /// one.
    ///
    /// # Errors
    ///
    /// [`SendError::BadName`] if `command` is not a valid identifier, and
    /// [`SendError::Rejected`] if the destination has no placement or the db
    /// refuses the write.
    pub async fn send_command(
        &self,
        dest: Sri,
        command: &str,
        payload: Vec<u8>,
    ) -> Result<(), SendError> {
        let cmd = Command::try_from(command).map_err(|_| SendError::BadName)?;
        self.exchange(DbClientRequest::SendCommand {
            dest_sri: dest,
            command: MailboxCommand {
                cmd,
                payload: Some(payload),
                attachment: self.attachment(),
            },
        })
        .await
    }

    /// Publishes an event, stamped as from this cell.
    ///
    /// # Errors
    ///
    /// [`SendError::BadName`] if `event` is not a valid identifier, and
    /// [`SendError::Rejected`] if the db refuses the write.
    pub async fn publish_event(&self, event: &str, payload: Vec<u8>) -> Result<(), SendError> {
        let ev = Event::try_from(event).map_err(|_| SendError::BadName)?;
        self.exchange(DbClientRequest::PublishEvent {
            event: MailboxEvent {
                event: ev,
                payload,
                attachment: self.attachment(),
            },
        })
        .await
    }

    /// Subscribes this cell to `event`, so published instances arrive via
    /// [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// [`SendError::BadName`] if `event` is not a valid identifier, and
    /// [`SendError::Rejected`] if the db refuses the subscription.
    pub async fn subscribe(&self, event: &str) -> Result<(), SendError> {
        let ev = Event::try_from(event).map_err(|_| SendError::BadName)?;
        self.exchange(DbClientRequest::SubscribeEvent(ev)).await
    }

    /// Stops delivering `event` to this cell.
    ///
    /// # Errors
    ///
    /// [`SendError::BadName`] if `event` is not a valid identifier, and
    /// [`SendError::Rejected`] if the db refuses the change.
    pub async fn unsubscribe(&self, event: &str) -> Result<(), SendError> {
        let ev = Event::try_from(event).map_err(|_| SendError::BadName)?;
        self.exchange(DbClientRequest::UnsubscribeEvent(ev)).await
    }

    fn attachment(&self) -> CellAttachment {
        let mut attachment = CellAttachment::default();
        attachment.set_sender(Some(self.sri.as_uuid()));
        attachment
    }

    async fn exchange(&self, req: DbClientRequest) -> Result<(), SendError> {
        self.net.requests.send(req).await;
        match self.net.responses.receive().await {
            DbClientResponse::SendCommand(result) | DbClientResponse::PublishEvent(result) => {
                result.map_err(|_| SendError::Rejected)
            }
            DbClientResponse::SubscribeEvent | DbClientResponse::UnsubscribeEvent => Ok(()),
            _ => Err(SendError::Rejected),
        }
    }
}

/// Something addressed to this cell.
#[derive(Debug)]
pub enum Message {
    /// A command sent by another cell.
    Command {
        /// The command name.
        command: Command,
        /// Its payload, empty if the sender supplied none.
        payload: Vec<u8>,
        /// The sending cell, if it identified itself.
        sender: Option<Sri>,
    },
    /// An event this cell subscribed to.
    Event {
        /// The event name.
        event: Event,
        /// Its payload.
        payload: Vec<u8>,
        /// The publishing cell, if it identified itself.
        sender: Option<Sri>,
    },
}

/// Why a send did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The command or event name is not a valid identifier.
    BadName,
    /// The swarm refused it — no placement for the destination, or a db error.
    Rejected,
}

impl core::fmt::Display for SendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadName => f.write_str("not a valid command or event name"),
            Self::Rejected => f.write_str("the swarm rejected the request"),
        }
    }
}
