use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::future::pending;

use cell_protocol::supervision::{FencingState, LeaseTracker, WatchedCell};
use cell_protocol::{
    DEPLOYMENT_TABLE, MESSAGES_TABLE, Sri, WatchdogResetReport, scope_of_cell, scope_of_deployment,
};
use db_client::application::Application;
use db_client::v1::models::{Cursor, Id, Subject};
use db_client::v1::{Client, Subscription};
use embassy_futures::select::{Either, Either6, select, select6};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Timer};
use myrmic_common::cells::{Command, Event};
use wasm_runtime::async_request::{
    CELL_MSG_CHANNEL, DbClientRequest, DbClientResponse, command_handled, reset_command_handled,
};
use zenoh_nano::session::Session;

use wasm_runtime::WasmTransfer;

use crate::myrmic;
use crate::myrmic::REGISTRATION_PERIOD;
use crate::{deploy, mailbox, requests, supervision};

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Safety-net fallback for the deployment and mailbox subscription pokes.
///
/// DB events are best-effort "pokes", but a poke can only be silently missed while the session is
/// disconnected: the dispatcher re-declares subscriptions on reconnect, so a live session always
/// resumes delivery. That silent window is therefore bounded by the session lease, so we poll once
/// per lease (plus a small margin, so a reconnect has re-declared and normal pokes have resumed
/// first) to sweep up anything that committed while we were down.
/// This is a backstop, not the primary trigger - the subscription is. Derived
/// at runtime from the session lease the firmware passes in.
fn subscription_fallback_period(session_lease: Duration) -> Duration {
    Duration::from_secs(session_lease.as_secs() + 5)
}
const EVENTS_POLL_PERIOD: Duration = Duration::from_millis(250);
/// Fencing verification cadence (spec §3). Slower than the Linux exec's 10s
/// to respect the radio budget: three point reads per pass while hosting a
/// cell (own row, parent row, parent node's lease; none while idle), giving
/// parent-death detection within roughly a minute of the edge's grace.
const VERIFY_PERIOD: Duration = Duration::from_secs(30);
/// Back-off applied when a subscription's `recv` errors, so the fallback timer
/// can take over instead of the branch busy-looping on the error.
const SUB_ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// The DB service loop. `session_lease` is the zenoh session lease the
/// firmware configured (it sizes the subscription fallback timers); `wall_time`
/// reads the swarm-synced wall clock, which stamps the node-lease sequence.
#[allow(
    clippy::too_many_lines,
    reason = "The select loop reads clearest in one place"
)]
pub async fn service(
    session: Session<'static, NoopRawMutex>,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    db_requests: Receiver<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    db_responses: Sender<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    session_lease: Duration,
    wall_time: fn() -> Option<core::time::Duration>,
) {
    let subscription_fallback_period = subscription_fallback_period(session_lease);
    log::trace!("[db-client] Started");

    let client = Client::new(&session);
    let zid = session.zid().await;

    // Context
    let mut cell: Option<(Sri, Vec<Command>)> = None;
    let mut last_deploy_id = None;
    let mut subscribed_events: Vec<(Event, Cursor)> = Vec::new();
    let mut awaiting_deletion_confirmation: bool = false;

    // The batched transaction of the cell function currently running. The
    // runtime opens one before it dispatches and closes it when the function
    // returns, so it is `None` between calls.
    let mut application: Option<Application> = None;

    // At most one mailbox command is with the runtime at a time, and the id of the last one handed
    // over, so a command whose handler failed is not immediately re-offered.
    let mut command_in_flight = false;
    let mut last_delivered: Option<Id> = None;

    // Child-side fencing (spec §3): the decision core is shared with the
    // Linux exec via `cell_protocol::supervision`; evidence is gathered on
    // the verify tick below.
    let mut watched: Option<WatchedCell> = None;
    let mut fencing = FencingState::new();
    let mut lease_tracker = LeaseTracker::new();
    let mut owed_cleanup: Vec<Sri> = Vec::new();
    // Rows left behind by a previous boot are swept once per boot.
    let mut swept = false;
    // A boot returns this node to its flashed tags: the overlay a previous run
    // was given is dropped before any is honoured again.
    let mut overlay_cleared = false;

    // Deployment notifications arrive via a DB subscription on the exec's deployment table
    let mut deployment_sub = declare_subscription(
        &client,
        Subject::Scope(scope_of_deployment(zid.to_string())),
        DEPLOYMENT_TABLE,
        "deployments",
    )
    .await;

    // Mailbox (command) subscription on the deployed cell's messages table. (Re)Declared whenever
    // the cell changes
    let mut mailbox_sub: Option<Subscription> = None;
    let mut mailbox_sub_sri: Option<Sri> = None;

    // Absolute deadlines for the fallback pollers.
    let mut next_deployment_fallback = Instant::now();
    let mut next_mailbox_fallback = Instant::now();
    let mut next_events_poll = Instant::now();
    let mut next_exec_registration = Instant::now();
    let mut next_lease_renewal = Instant::now();
    let mut next_verify = Instant::now();
    let mut registered_epoch = session.connection_epoch();

    loop {
        // Liveness (required): the deployment poll guarantees an iteration at
        // least every DEPLOYMENT_POLL_PERIOD, so a stalled bump means the
        // node's workload pump is wedged.
        esp_watchdog::liveness::bump(esp_watchdog::liveness::Task::DbClient);

        // Re-register and re-lease promptly after a transport reconnect: the
        // session drop undeclared our liveliness token (deregistering this
        // node's exec row), and every missed renewal eats into the lease ttl
        // observers judge us by. Waiting out the full periods would lose.
        let epoch = session.connection_epoch();
        if epoch != registered_epoch {
            registered_epoch = epoch;
            next_exec_registration = Instant::now();
            next_lease_renewal = Instant::now();
        }

        // Keep the mailbox subscription aligned with the currently deployed cell, reading it right
        // away on a change so a command already queued is not missed before the first poke.
        resync_mailbox_subscription(
            &client,
            cell.as_ref(),
            &mut mailbox_sub,
            &mut mailbox_sub_sri,
            &mut next_mailbox_fallback,
            &mut last_delivered,
            &mut command_in_flight,
        )
        .await;

        // Deployment: wait for a subscription poke, otherwise the fallback timer.
        let deployment_fut = wait_poke_or_fallback(
            deployment_sub.as_mut(),
            next_deployment_fallback,
            "deployment",
        );

        // Mailbox: park while no cell is deployed. While a command is in flight, wait for the
        // runtime to finish with it instead of reading — the message is only removed inside that
        // handler's transaction, so reading now would deliver it twice. Otherwise wait for a poke
        // (or the fallback timer), degrading to timer-only polling if the subscription failed.
        let mailbox_fut = async {
            if cell.is_none() {
                pending::<()>().await;
            } else if command_in_flight {
                command_handled().await;
            } else {
                wait_poke_or_fallback(mailbox_sub.as_mut(), next_mailbox_fallback, "mailbox").await;
            }
        };

        // Events: park unless the cell is subscribed to at least one event.
        let events_fut = async {
            if subscribed_events.is_empty() {
                pending::<()>().await;
            } else {
                Timer::at(next_events_poll).await;
            }
        };

        match select6(
            deployment_fut,
            db_requests.receive(),
            mailbox_fut,
            events_fut,
            Timer::at(next_exec_registration.min(next_lease_renewal)),
            Timer::at(next_verify),
        )
        .await
        {
            // Deployment notification (or fallback)
            Either6::First(()) => {
                deploy::handle(
                    &client,
                    zid,
                    &mut last_deploy_id,
                    wasm_transfer,
                    cell.as_ref(),
                    &mut awaiting_deletion_confirmation,
                    &mut watched,
                )
                .await;
                next_deployment_fallback = Instant::now() + subscription_fallback_period;
            }
            Either6::Second(db_client_req) => {
                requests::handle(
                    &client,
                    zid,
                    db_client_req,
                    &mut cell,
                    db_responses,
                    &mut subscribed_events,
                    &mut awaiting_deletion_confirmation,
                    &mut watched,
                    &mut application,
                )
                .await;
            }
            // Mailbox command notification, fallback tick, or the runtime finishing with the
            // command in flight.
            Either6::Third(()) => {
                if command_in_flight {
                    command_in_flight = false;
                } else {
                    next_mailbox_fallback = Instant::now() + subscription_fallback_period;
                    // A fresh poke or fallback tick licenses another attempt at whatever is at the
                    // head, including a command whose handler just failed.
                    last_delivered = None;
                }

                if let Some((sri, _)) = cell.as_ref() {
                    command_in_flight =
                        forward_next_command(&client, sri, &mut last_delivered).await;
                }
            }
            // Subscribed-event polling
            Either6::Fourth(()) => {
                next_events_poll = Instant::now() + EVENTS_POLL_PERIOD;
                forward_events(&client, &mut subscribed_events).await;
            }
            // Node maintenance: exec re-registration and lease renewal,
            // whichever is due (the timer fires at the earlier deadline).
            Either6::Fifth(()) => {
                if Instant::now() >= next_exec_registration {
                    next_exec_registration =
                        Instant::now() + Duration::from_secs(REGISTRATION_PERIOD.as_secs());
                    // Make ourselves available in the myrmic network as an exec runtime
                    let runtime_info = myrmic::create_runtime_info(
                        &client,
                        session.zid().await,
                        &mut overlay_cleared,
                    )
                    .await;
                    myrmic::register_exec_runtime(&client, runtime_info).await;

                    // If this boot followed a watchdog reset, report it to the swarm
                    // (SDS-FEAT-2026-HWD-001 Area D). The report is held until the
                    // swarm accepted it, so a write that could not reach a database
                    // is reattempted on every following registration round - that
                    // round is already the node's "is the data layer usable" probe,
                    // so the retry rides on it instead of on a timer of its own.
                    if let Some(boot) = esp_watchdog::watchdog::peek_boot_report() {
                        let report = WatchdogResetReport {
                            device_id: myrmic::device_id(),
                            runtime_id: zid.into(),
                            reset_count: boot.reset_count,
                            last_reason: boot.reason,
                            last_uptime_ms: boot.uptime_ms,
                            stale_tasks: esp_watchdog::liveness::names_of(boot.stale_mask)
                                .map(String::from)
                                .collect(),
                        };
                        match myrmic::report_watchdog_reset(&client, report).await {
                            Ok(()) => esp_watchdog::watchdog::clear_boot_report(&boot),
                            Err(e) => log::error!(
                                "[watchdog] reset report #{} not delivered, retrying in {} s: {e}",
                                boot.reset_count,
                                REGISTRATION_PERIOD.as_secs(),
                            ),
                        }
                    }
                }

                // Renewed right after the boot registration: that exchange
                // taught the clock swarm time, which the lease seq needs. Held
                // back until the boot sweep completes: a fresh lease makes this
                // exec a placement target, so leasing first would let a deploy
                // land before the sweep and be mistaken for a previous-boot
                // remnant and killed. Rechecked on the retry cadence meanwhile.
                if Instant::now() >= next_lease_renewal {
                    next_lease_renewal = Instant::now()
                        + if !swept {
                            myrmic::LEASE_RETRY_PERIOD
                        } else if myrmic::renew_node_lease(&client, zid, wall_time).await {
                            myrmic::LEASE_RENEW_PERIOD
                        } else {
                            myrmic::LEASE_RETRY_PERIOD
                        };
                }
            }
            // Fencing verification pass
            Either6::Sixth(()) => {
                next_verify = Instant::now() + VERIFY_PERIOD;
                if !swept {
                    swept = supervision::boot_sweep(
                        &client,
                        zid,
                        cell.as_ref().map(|(sri, _)| sri),
                        &mut owed_cleanup,
                    )
                    .await;
                }
                supervision::verify_tick(
                    &client,
                    zid,
                    cell.is_some(),
                    &mut watched,
                    &mut lease_tracker,
                    &mut fencing,
                    &mut owed_cleanup,
                )
                .await;
            }
        }
    }
}

/// (Re)declares the mailbox subscription so it always targets the currently deployed cell.
/// `mailbox_sub_sri` tracks which cell the live subscription points at; when it diverges from
/// `cell` we drop the stale subscription and declare a fresh one, and arrange for the mailbox to be
/// read straight away so a command already queued is not missed before the first poke.
async fn resync_mailbox_subscription(
    client: &Client,
    cell: Option<&(Sri, Vec<Command>)>,
    mailbox_sub: &mut Option<Subscription>,
    mailbox_sub_sri: &mut Option<Sri>,
    next_mailbox_fallback: &mut Instant,
    last_delivered: &mut Option<Id>,
    command_in_flight: &mut bool,
) {
    let current_sri = cell.map(|(sri, _)| *sri);
    if current_sri == *mailbox_sub_sri {
        return;
    }

    // Drop any stale subscription (its `Drop` unregisters it).
    *mailbox_sub = None;
    // Nothing the previous cell was doing carries over: its head message says
    // nothing about this one's, and its completion is not one to wait for.
    *last_delivered = None;
    *command_in_flight = false;

    let Some((sri, _)) = cell else {
        // No cell deployed.
        *mailbox_sub_sri = None;
        return;
    };

    *mailbox_sub = declare_subscription(
        client,
        Subject::Scope(scope_of_cell(sri)),
        MESSAGES_TABLE,
        "mailbox",
    )
    .await;
    if mailbox_sub.is_some() {
        // Only record the target once the subscription actually exists. If a failed declare leaves
        // `mailbox_sub_sri` stale the next iteration will retry.
        *mailbox_sub_sri = current_sri;
        // Due now, so the mailbox arm reads without waiting for a poke.
        *next_mailbox_fallback = Instant::now();
    }
}

/// Hands the command at the head of the mailbox to the runtime.
///
/// Returns whether a command is now in flight. Only one is ever in flight: the message is removed
/// inside the transaction of the handler that runs it, so reading again beforehand would deliver it
/// twice. The wait for the runtime to finish is a *select arm*, never awaited here — this runs on the
/// task that also serves the handler's own db calls, so blocking on the handler would deadlock it.
///
/// A command already handed over and still at the head is refused: its handler failed, so retrying
/// it waits for the next poke or fallback tick rather than spinning on it here.
async fn forward_next_command(client: &Client, sri: &Sri, last_delivered: &mut Option<Id>) -> bool {
    let Some(cell_msg) = mailbox::next_command(client, sri).await else {
        *last_delivered = None;
        return false;
    };

    let msg_id = cell_msg.mailbox_entry().cloned();
    if msg_id.is_some() && msg_id == *last_delivered {
        return false;
    }

    // Arm the completion before the runtime can report against it, so a stale one
    // from an abandoned hand-over or a previous cell is never mistaken for ours.
    reset_command_handled();

    // A full channel is no loss: the command is still in the mailbox, so it is
    // simply read again on the next poke.
    if CELL_MSG_CHANNEL.try_send(cell_msg).is_err() {
        log::debug!("[db-client] command deferred — cell message channel full");
        *last_delivered = None;
        return false;
    }

    *last_delivered = msg_id;
    true
}

/// Drains every subscribed event with new entries into the cell message channel.
async fn forward_events(client: &Client, subscribed_events: &mut Vec<(Event, Cursor)>) {
    let sender = CELL_MSG_CHANNEL.sender();
    for cell_msg in mailbox::poll_for_events(client, subscribed_events).await {
        sender.send(cell_msg).await;
    }
}

/// Declares a DB subscription, logging and returning `None` on failure so the caller can degrade to
/// fallback polling. `name` labels the log message.
async fn declare_subscription(
    client: &Client,
    subject: Subject,
    table: &str,
    name: &str,
) -> Option<Subscription> {
    match client.subscribe(subject, table).await {
        Ok(sub) => Some(sub),
        Err(err) => {
            log::warn!("[db-client] Failed to subscribe to {name}, falling back to polling: {err}");
            None
        }
    }
}

/// Waits for a subscription poke, falling back to the `fallback` deadline.
///
/// DB events are best-effort pokes, so the fallback timer is what guarantees progress if one is
/// missed. If `sub` is `None` (declaration failed) only the timer is used, and a `recv` error
/// backs off before retrying so a broken subscription cannot spin the select loop.
async fn wait_poke_or_fallback(sub: Option<&mut Subscription>, fallback: Instant, name: &str) {
    match sub {
        Some(sub) => match select(sub.recv(), Timer::at(fallback)).await {
            Either::First(Ok(_)) | Either::Second(()) => {}
            Either::First(Err(err)) => {
                log::warn!("[db-client] {name} subscription error: {err}");
                Timer::after(SUB_ERROR_BACKOFF).await;
            }
        },
        None => Timer::at(fallback).await,
    }
}
