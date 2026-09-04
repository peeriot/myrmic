use cell_protocol::Sri;
use myrmic_common::cells::Command;
use sorg_common::{OutgoingMessage, bail, custom_err, placement_exists};

use crate::{Client, Result};

impl Client {
    pub async fn command_send(
        &self,
        sri: Sri,
        cmd_name: &str,
        payload: Option<Vec<u8>>,
        trace: Option<(u128, u64)>,
    ) -> Result<()> {
        let session = self.session();
        let cmd: Command = cmd_name.try_into().map_err(|msg| custom_err!("{msg}"))?;
        if !placement_exists(session, &sri).await? {
            bail!("cell {sri} has no placement");
        }
        let mut msg = OutgoingMessage::command(&sri, &cmd, payload)?;
        msg.attach_span_context(trace);
        msg.send(session, None).await?;
        Ok(())
    }
}
