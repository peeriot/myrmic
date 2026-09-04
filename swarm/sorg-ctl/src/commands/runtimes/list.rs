use comfy_table::Table;
use sorg_client::Client;
use sorg_client::types::ExecutionCapabilities;
use std::fmt::Write;

use crate::{
    Result, print_info,
    utils::{AsCell, MAX_WIDTH_ID, new_table},
};

pub(crate) async fn list_exec_runtimes(client: Client) -> Result<()> {
    let exec_records = client.list_registered_execs().await?;
    if exec_records.is_empty() {
        print_info!("no execution runtimes found");
    } else {
        let mut runtime_table = new_table();
        runtime_table.add_row(vec!["ID", "Name", "Capabilities"]);
        for rt_record in exec_records {
            let id_cell = format!("{id}", id = rt_record.id()).cell_prefix(MAX_WIDTH_ID);
            let name_cell = rt_record.name().unwrap_or("").cell_prefix(10);
            let capa_cell = exec_capa_table(rt_record.capabilities()).cell();
            runtime_table.add_row(vec![id_cell, name_cell, capa_cell]);
        }
        println!("{runtime_table}");
    }
    Ok(())
}

fn exec_capa_table(exec_capas: &ExecutionCapabilities) -> Table {
    let mut table = new_table();
    if !exec_capas.tags().is_empty() {
        let mut tag_string = String::new();
        let n_tags = exec_capas.tags().len();
        for (idx, tag) in exec_capas.tags().iter().enumerate() {
            write!(&mut tag_string, "{t}", t = tag.as_ref()).expect("writing into string is fine");
            if idx < n_tags - 1 {
                writeln!(&mut tag_string).expect("writing string is fine");
            }
        }
        table.add_row(vec!["Tags", &tag_string]);
    }
    table
}
