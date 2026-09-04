//! Emission of `cell_lost` notifications (spec §5): a reserved system
//! command into the parent's per-SRI mailbox. Db-backed, so it works
//! cross-exec and survives the parent's exec restarting.

use cell_mailbox::OutgoingMessage;
use cell_protocol::Sri;
use myrmic_common::cells::{CellLost, Command, LostReason, SYS_CELL_LOST};
use tracing::info;
use zenoh::Session;

use crate::cells::root_death;
use crate::{Result, custom_err};

/// Sends `note` to `parent`'s mailbox. Callers decide the boundary rules
/// (roots and detached edges get nothing); this just delivers and traces.
pub async fn emit_cell_lost(session: &Session, parent: &Sri, note: CellLost) -> Result<()> {
    info!(
        parent = %parent,
        cell = %note.cell,
        reason = ?note.reason,
        "emitting cell_lost"
    );
    let command = Command::new(SYS_CELL_LOST.to_owned())
        .map_err(|err| custom_err!("reserved command name invalid: {err}"))?;
    let payload =
        postcard::to_allocvec(&note).map_err(|err| custom_err!("encode cell_lost: {err}"))?;
    OutgoingMessage::command(parent, &command, Some(payload))
        .map_err(|err| custom_err!("build cell_lost command: {err}"))?
        .send(session, None)
        .await
        .map_err(|err| custom_err!("send cell_lost to '{parent}': {err}"))
}

/// Reports a cell's death down the right channel, given its spawn edge. The
/// single chokepoint every exec death site calls so the root case is handled
/// uniformly:
///
/// * **detached** — nothing (the parent opted out of this cell's lifetime).
/// * **has a parent** — a `cell_lost` note into the parent's mailbox (§5).
/// * **root** (`parent` is `None`) — a pending [`root_death`] signal the
///   orchestrator turns into a restart decision.
pub async fn report_cell_death(
    session: &Session,
    cell: Sri,
    gen_id: cell_protocol::Gen,
    parent: Option<Sri>,
    detached: bool,
    local_name: Option<String>,
    reason: LostReason,
) -> Result<()> {
    if detached {
        return Ok(());
    }
    match parent {
        Some(parent) => {
            let note = CellLost {
                cell,
                local_name,
                reason,
            };
            emit_cell_lost(session, &parent, note).await
        }
        None => root_death::record(session, cell, gen_id, reason).await,
    }
}
