use db_commons::models::{Cursor, Scope, Table, TbOrderBy, tb_list};

/// Lists the rows of `table` in `scope` after `from`. Without a cursor - the first
/// notification for a scope - only the newest row is read, so a backlog that predates the
/// stream is not replayed as fresh activity.
pub(crate) async fn list(
    db: &db_client::v1::Client,
    scope: Scope,
    table: Table,
    from: Option<Cursor>,
) -> anyhow::Result<tb_list::Response> {
    let op = list_op(scope.clone(), table, from);

    db.read_tx_in(scope, async move |client, tx_id| {
        Ok(client
            .send(tb_list::Request { id: tx_id, op })
            .await?
            .map_err(|err| anyhow::anyhow!("{}", err.message))?)
    })
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))
}

fn list_op(scope: Scope, table: Table, from: Option<Cursor>) -> tb_list::Op {
    let (limit, order) = match from {
        Some(_) => (None, None),
        None => (Some(1), Some(TbOrderBy::KeyDesc)),
    };

    tb_list::Op {
        scope,
        table,
        cursor: from,
        limit,
        order,
    }
}

#[cfg(test)]
mod tests {
    use db_commons::models::TbOrderBy;

    use super::{Cursor, Scope, list_op};

    #[test]
    fn the_first_read_of_a_scope_takes_only_the_newest_row() {
        let op = list_op(scope(), "messages".into(), None);

        assert_eq!(op.limit, Some(1));
        assert_eq!(op.order, Some(TbOrderBy::KeyDesc));
    }

    #[test]
    fn a_later_read_of_a_scope_takes_everything_after_the_cursor() {
        let op = list_op(scope(), "messages".into(), Some(Cursor::After(vec![7])));

        assert_eq!(op.cursor, Some(Cursor::After(vec![7])));
        assert_eq!(op.limit, None);
        assert_eq!(op.order, None);
    }

    fn scope() -> Scope {
        Scope::new("CELLS", "00000000-0000-0000-0000-000000000001", "p")
    }
}
