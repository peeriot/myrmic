use sorg_client::Client;

use crate::{
    Result, print_info,
    utils::{AsCell, MAX_WIDTH_ID, new_table},
};

pub(crate) async fn list_orch_runtimes(client: Client) -> Result<()> {
    let orch_records = client.list_orch_runtimes().await?;
    if orch_records.is_empty() {
        print_info!("no orchestration runtimes found");
    } else {
        let mut runtime_table = new_table();
        runtime_table.add_row(vec!["ID", "Capabilities"]);
        for orch_record in orch_records {
            let id_cell = format!("{id}", id = orch_record.id).cell_prefix(MAX_WIDTH_ID);
            runtime_table.add_row(vec![id_cell, "".cell()]);
        }
        println!("{runtime_table}");
    }
    Ok(())
}
