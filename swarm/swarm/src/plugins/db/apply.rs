//! Applying operations to a transaction.
//!
//! One function per [`TxOp`], each a pure `(transaction, op) -> response`, so an
//! operation behaves identically whether it arrived alone or as part of a
//! batched application.

// The uniform `(transaction, op)` shape is worth more here than saving a move
// for the ops that only read their fields.
#![allow(clippy::needless_pass_by_value)]

use db::domain::Hash;
use db_commons::models::*;

use super::TxEvents;

type StoreTx = db::store::fjall::Transaction<TxEvents>;

const DEFAULT_PAGE_SIZE: usize = 100;

pub fn format_error(err: &anyhow::Error) -> String {
    use std::fmt::Write as _;

    let mut out = format!("{}", err);
    for cause in err.chain().skip(1) {
        let _ = write!(out, "\nCaused by: {}", cause);
    }
    out
}

/// Process-wide monotonic counter for generated entity ids, mirroring the shared
/// context `Uuid::now_v7` uses internally. Because several inserts can share one
/// transaction timestamp (a batch, or same-millisecond requests), the counter is
/// what keeps their ids distinct and sortable rather than colliding.
static EID_CONTEXT: std::sync::Mutex<uuid::ContextV7> =
    std::sync::Mutex::new(uuid::ContextV7::new());

/// Build a v7 entity id from the transaction's hybrid-logical-clock timestamp, so
/// generated ids order by the transaction's clock rather than an independent
/// wall-clock read.
pub fn eid_from_ts(ts: uhlc::Timestamp) -> uuid::Uuid {
    let dur = ts.get_time().to_duration();
    uuid::Uuid::new_v7(uuid::Timestamp::from_unix(
        &EID_CONTEXT,
        dur.as_secs(),
        dur.subsec_nanos(),
    ))
}

fn key_scope(scope: &Scope) -> db::domain::Scope<'_> {
    db::domain::Key::new_scope(&scope.namespace, &scope.database, &scope.schema)
}

/// Applies one operation, leaving the transaction uncommitted.
///
/// An `Err` aborts the whole application: the caller rolls the transaction back
/// and reports which op failed.
pub fn apply(tx: &mut StoreTx, op: TxOp) -> Result<TxOpResponse, String> {
    Ok(match op {
        TxOp::ScopeBackup(op) => scope_backup(tx, op)?.into(),
        TxOp::ScopeRestore(op) => scope_restore(tx, op)?.into(),
        TxOp::KeyPut(op) => key_put(tx, op)?.into(),
        TxOp::KeyGet(op) => key_get(tx, op)?.into(),
        TxOp::KeyDelete(op) => key_delete(tx, op)?.into(),
        TxOp::KeyPrefix(op) => key_prefix(tx, op)?.into(),
        TxOp::TbInsert(op) => tb_insert(tx, op)?.into(),
        TxOp::TbAppend(op) => tb_append(tx, op)?.into(),
        TxOp::TbInsertBatched(op) => tb_insert_batched(tx, op)?.into(),
        TxOp::TbCount(op) => tb_count(tx, op)?.into(),
        TxOp::TbGet(op) => tb_get(tx, op)?.into(),
        TxOp::TbList(op) => tb_list(tx, op)?.into(),
        TxOp::TbDelete(op) => tb_delete(tx, op)?.into(),
        TxOp::TsPublish(op) => ts_publish(tx, op)?.into(),
        TxOp::TsFind(op) => ts_find(tx, op)?.into(),
        TxOp::BlobStore(op) => blob_store(tx, op)?.into(),
        TxOp::BlobLink(op) => blob_link(tx, op)?.into(),
        TxOp::BlobUnlink(op) => blob_unlink(tx, op)?.into(),
        TxOp::BlobMove(op) => blob_move(tx, op)?.into(),
        TxOp::BlobResolve(op) => blob_resolve(tx, op)?.into(),
        TxOp::PathResolve(op) => path_resolve(tx, op)?.into(),
        TxOp::PathsList(op) => paths_list(tx, op)?.into(),
        TxOp::SemUpdate(op) => sem_update(tx, op)?.into(),
        TxOp::SemSelect(op) => sem_select(tx, op)?.into(),
        TxOp::SemAsk(op) => sem_ask(tx, op)?.into(),
        TxOp::SemConstruct(op) => sem_construct(tx, op)?.into(),
        TxOp::SemDescribe(op) => sem_describe(tx, op)?.into(),
    })
}

fn scope_backup(tx: &mut StoreTx, op: scope_backup::Op) -> Result<scope_backup::Response, String> {
    use anyhow::Context as _;

    let snapshot = tx
        .take_snapshot(key_scope(&op.scope))
        .context("unable to take snapshot")
        .map_err(|err| format_error(&err))?;

    Ok(scope_backup::Response { snapshot })
}

fn scope_restore(
    tx: &mut StoreTx,
    op: scope_restore::Op,
) -> Result<scope_restore::Response, String> {
    use anyhow::Context as _;

    tx.restore_snapshot(key_scope(&op.scope), op.snapshot)
        .context("unable to restore snapshot")
        .map_err(|err| format_error(&err))?;

    Ok(scope_restore::Response {})
}

fn key_put(tx: &mut StoreTx, op: key_put::Op) -> Result<key_put::Response, String> {
    let key = key_scope(&op.scope).kv(&op.key);

    tx.key_put(key, &op.value)
        .map_err(|err| format_error(&err))?;

    Ok(key_put::Response {})
}

fn key_get(tx: &mut StoreTx, op: key_get::Op) -> Result<key_get::Response, String> {
    let key = key_scope(&op.scope).kv(&op.key);

    let value = tx.key_get(key).map_err(|err| format_error(&err))?;

    Ok(key_get::Response { value })
}

fn key_delete(tx: &mut StoreTx, op: key_delete::Op) -> Result<key_delete::Response, String> {
    let key = key_scope(&op.scope).kv(&op.key);

    tx.key_delete(key).map_err(|err| format_error(&err))?;

    Ok(key_delete::Response {})
}

fn key_prefix(tx: &mut StoreTx, op: key_prefix::Op) -> Result<key_prefix::Response, String> {
    let keys = tx
        .key_prefix(key_scope(&op.scope), &op.prefix)
        .map_err(|err| format_error(&err))?;

    Ok(key_prefix::Response { keys })
}

/// Inserts a row and reports the id it landed under. `eid: None` mints one from
/// the transaction's timestamp, which is why the mint stays server-side: ids
/// order by the transaction's clock, and the window between minting and
/// committing stays as short as the application is.
fn tb_insert(tx: &mut StoreTx, op: tb_insert::Op) -> Result<tb_insert::Response, String> {
    let eid = insert_row(tx, op.scope, op.table, op.eid, &op.value)?;

    Ok(tb_insert::Response { eid })
}

/// [`tb_insert()`] without reporting the id back — the deferrable form.
fn tb_append(tx: &mut StoreTx, op: tb_append::Op) -> Result<tb_append::Response, String> {
    insert_row(tx, op.scope, op.table, op.eid, &op.value)?;

    Ok(tb_append::Response {})
}

fn insert_row(
    tx: &mut StoreTx,
    scope: Scope,
    table: Table,
    eid: Option<Id>,
    value: &[u8],
) -> Result<Id, String> {
    let tb = key_scope(&scope).table(&table);

    let now;
    let eid = if let Some(eid) = eid.as_ref() {
        eid.as_slice()
    } else {
        now = eid_from_ts(tx.timestamp());
        now.as_bytes()
    };

    tx.tb_insert(tb, eid, value)
        .map_err(|err| format_error(&err))?;

    let eid = eid.to_vec();
    tx.metadata_or_default().insert((scope, table));

    Ok(eid)
}

fn tb_insert_batched(
    tx: &mut StoreTx,
    op: tb_insert_batched::Op,
) -> Result<tb_insert_batched::Response, String> {
    let tb = key_scope(&op.scope).table(&op.table);

    // Resolve ids up front (allocating one for each missing entry) so they
    // outlive the borrowed batch and can be returned to the caller in order.
    let ts = tx.timestamp();
    let eids: Vec<Vec<u8>> = op
        .entries
        .iter()
        .map(|(eid, _)| match eid {
            Some(eid) => eid.clone(),
            None => eid_from_ts(ts).as_bytes().to_vec(),
        })
        .collect();

    let batch: Vec<(&[u8], &[u8])> = eids
        .iter()
        .zip(op.entries.iter())
        .map(|(eid, (_, value))| (eid.as_slice(), value.as_slice()))
        .collect();

    tx.tb_insert_batched(tb, &batch)
        .map_err(|err| format_error(&err))?;
    drop(batch);

    if !eids.is_empty() {
        tx.metadata_or_default().insert((op.scope, op.table));
    }

    Ok(tb_insert_batched::Response { eids })
}

fn tb_count(tx: &mut StoreTx, op: tb_count::Op) -> Result<tb_count::Response, String> {
    let tb = key_scope(&op.scope).table(&op.table);

    let count = tx.tb_count(tb).map_err(|err| format_error(&err))?;

    Ok(tb_count::Response { count })
}

fn tb_get(tx: &mut StoreTx, op: tb_get::Op) -> Result<tb_get::Response, String> {
    let tb = key_scope(&op.scope).table(&op.table);

    let value = tx.tb_get(tb, &op.eid).map_err(|err| format_error(&err))?;

    Ok(tb_get::Response { value })
}

fn tb_list(tx: &mut StoreTx, op: tb_list::Op) -> Result<tb_list::Response, String> {
    let tb = key_scope(&op.scope).table(&op.table);

    let entities = tx
        .tb_list(tb, op.cursor, op.limit, op.order)
        .map_err(|err| format_error(&err))?;

    Ok(tb_list::Response { entities })
}

fn tb_delete(tx: &mut StoreTx, op: tb_delete::Op) -> Result<tb_delete::Response, String> {
    let tb = key_scope(&op.scope).table(&op.table);

    tx.tb_delete(tb, &op.eid)
        .map_err(|err| format_error(&err))?;

    Ok(tb_delete::Response {})
}

fn ts_publish(tx: &mut StoreTx, op: ts_publish::Op) -> Result<ts_publish::Response, String> {
    tx.publish_measurement(
        key_scope(&op.scope),
        &op.measurement,
        op.tags,
        op.fields,
        op.timestamp,
    )
    .map_err(|err| format_error(&err))?;

    Ok(ts_publish::Response {})
}

fn ts_find(tx: &mut StoreTx, op: ts_find::Op) -> Result<ts_find::Response, String> {
    let samples = tx
        .find_measurement(
            key_scope(&op.scope),
            &op.measurement,
            op.limit,
            op.start,
            op.end,
            op.order,
        )
        .map_err(|err| format_error(&err))?;

    Ok(ts_find::Response { samples })
}

fn blob_store(tx: &mut StoreTx, op: blob_store::Op) -> Result<blob_store::Response, String> {
    let blob_id = tx
        .store_blob(key_scope(&op.scope), &op.blob)
        .map_err(|err| format_error(&err))?;

    let hash = match blob_id.hash {
        Hash::Sha2(hash) => BlobHash::Sha2(hash),
    };

    Ok(blob_store::Response {
        blob_id: BlobId {
            scope: op.scope,
            hash,
        },
    })
}

fn blob_link(tx: &mut StoreTx, op: blob_link::Op) -> Result<blob_link::Response, String> {
    let scope = key_scope(&op.blob_id.scope);
    let path = scope.path(&op.path);

    let hash = match op.blob_id.hash {
        BlobHash::Sha2(hash) => Hash::Sha2(hash),
    };

    tx.link_blob(path, scope.blob_id(hash))
        .map_err(|err| format_error(&err))?;

    Ok(blob_link::Response {})
}

fn blob_unlink(tx: &mut StoreTx, op: blob_unlink::Op) -> Result<blob_unlink::Response, String> {
    let path = key_scope(&op.scope).path(&op.path);

    tx.unlink_blob(path).map_err(|err| format_error(&err))?;

    Ok(blob_unlink::Response {})
}

fn blob_move(tx: &mut StoreTx, op: blob_move::Op) -> Result<blob_move::Response, String> {
    let scope = key_scope(&op.scope);
    let old_path = scope.path(&op.old_path);
    let new_path = scope.path(&op.new_path);

    tx.move_blob(old_path, new_path)
        .map_err(|err| format_error(&err))?;

    Ok(blob_move::Response {})
}

fn blob_resolve(tx: &mut StoreTx, op: blob_resolve::Op) -> Result<blob_resolve::Response, String> {
    let scope = key_scope(&op.blob_id.scope);

    let hash = match op.blob_id.hash {
        BlobHash::Sha2(hash) => Hash::Sha2(hash),
    };

    let blob_id = scope.blob_id(hash);
    let blob = tx.resolve_blob(blob_id).map_err(|err| format_error(&err))?;

    let blob_id = BlobId {
        hash: match blob_id.hash {
            Hash::Sha2(blob_hash) => BlobHash::Sha2(blob_hash),
        },
        scope: Scope {
            namespace: blob_id.namespace.to_owned(),
            database: blob_id.database.to_owned(),
            schema: blob_id.schema.to_owned(),
        },
    };

    let Some(blob) = blob else {
        return Ok(blob_resolve::Response { blob: None });
    };

    let blob = chunk_of(blob, blob_id, op.range)?;

    Ok(blob_resolve::Response { blob: Some(blob) })
}

fn path_resolve(tx: &mut StoreTx, op: path_resolve::Op) -> Result<path_resolve::Response, String> {
    let path = key_scope(&op.scope).path(&op.path);

    let resolved = tx.resolve_path(path).map_err(|err| format_error(&err))?;

    let Some((blob, blob_id)) = resolved else {
        return Ok(path_resolve::Response { blob: None });
    };

    let blob_id = BlobId {
        hash: match blob_id.hash {
            Hash::Sha2(blob_hash) => BlobHash::Sha2(blob_hash),
        },
        scope: Scope {
            namespace: blob_id.namespace.to_owned(),
            database: blob_id.database.to_owned(),
            schema: blob_id.schema.to_owned(),
        },
    };

    let blob = chunk_of(blob, blob_id, op.range)?;

    Ok(path_resolve::Response { blob: Some(blob) })
}

/// Cuts the requested range out of a resolved blob. `None` returns all of it; a
/// zero-length range returns only the id and total length, which is how callers
/// check existence without shipping the bytes.
#[allow(clippy::cast_possible_truncation)] // Supporting only 64-bit targets
fn chunk_of(
    blob: Blob,
    blob_id: BlobId,
    range: Option<ChunkRange>,
) -> Result<BlobResponse, String> {
    let total_len = blob.len() as u64;

    let Some(ChunkRange { offset, length }) = range else {
        return Ok(BlobResponse {
            total_len,
            blob,
            blob_id,
            range: None,
        });
    };

    // Echoes the offset asked about rather than zeroing it: the returned range
    // is what identifies the reply, and a caller paging by echoing it back —
    // or asserting the reply matches its request — cannot tell which offset an
    // empty answer belongs to otherwise.
    if length == 0 {
        return Ok(BlobResponse {
            total_len,
            blob: Vec::new(),
            blob_id,
            range: Some(ChunkRange { offset, length: 0 }),
        });
    }

    let start = offset as usize;
    let end = start.saturating_add(length as usize).min(blob.len());

    let Some(chunk) = blob.get(start..end) else {
        return Err(format!(
            "requested chunk {offset}..{} is out of bounds for a blob of {total_len} bytes",
            offset.saturating_add(length),
        ));
    };

    Ok(BlobResponse {
        range: Some(ChunkRange {
            offset,
            length: chunk.len() as u64,
        }),
        blob: chunk.to_vec(),
        blob_id,
        total_len,
    })
}

fn paths_list(tx: &mut StoreTx, op: paths_list::Op) -> Result<paths_list::Response, String> {
    let paths = tx
        .list_paths(key_scope(&op.scope), op.limit)
        .map_err(|err| format_error(&err))?;

    Ok(paths_list::Response { paths })
}

fn sem_update(tx: &mut StoreTx, op: sem_update::Op) -> Result<sem_update::Response, String> {
    let update = db::semantic::Update::parse(&op.query, op.base_iri.as_deref())
        .map_err(|err| format!("{}", err))?;

    tx.sem_update(key_scope(&op.scope), update)
        .map_err(|err| format_error(&err))?;

    Ok(sem_update::Response {})
}

/// Parses `query` and rejects anything but the expected form — a `SELECT` sent
/// to `sem_ask` would otherwise fail deeper in with a worse message.
fn sem_query(
    query: &str,
    base_iri: Option<&str>,
    expected: &'static str,
    is_expected: fn(&db::semantic::QueryKind) -> bool,
) -> Result<db::semantic::Query, String> {
    let query = db::semantic::Query::parse(query, base_iri).map_err(|err| format!("{}", err))?;

    if !is_expected(query.kind()) {
        return Err(format!(
            "Expected `{expected}` query, got `{}`",
            query.kind().name()
        ));
    }

    Ok(query)
}

fn sem_select(tx: &mut StoreTx, op: sem_select::Op) -> Result<sem_select::Response, String> {
    let query = sem_query(&op.query, op.base_iri.as_deref(), "SELECT", |kind| {
        matches!(kind, db::semantic::QueryKind::Select)
    })?;

    let skip = op.skip.unwrap_or_default();
    let limit = op.limit.unwrap_or(DEFAULT_PAGE_SIZE);

    let db::semantic::QuerySolution {
        variables,
        solutions,
    } = tx
        .sem_solution(key_scope(&op.scope), query, skip, limit)
        .map_err(|err| format_error(&err))?;

    let solutions = solutions
        .into_iter()
        .map(|solution| {
            solution
                .into_iter()
                .map(|term| term.as_ref().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    Ok(sem_select::Response {
        variables,
        solutions,
    })
}

fn sem_ask(tx: &mut StoreTx, op: sem_ask::Op) -> Result<sem_ask::Response, String> {
    let query = sem_query(&op.query, op.base_iri.as_deref(), "ASK", |kind| {
        matches!(kind, db::semantic::QueryKind::Ask)
    })?;

    let answer = tx
        .sem_ask(key_scope(&op.scope), query)
        .map_err(|err| format_error(&err))?;

    Ok(sem_ask::Response { answer })
}

fn sem_construct(
    tx: &mut StoreTx,
    op: sem_construct::Op,
) -> Result<sem_construct::Response, String> {
    let query = sem_query(&op.query, op.base_iri.as_deref(), "CONSTRUCT", |kind| {
        matches!(kind, db::semantic::QueryKind::Construct(_))
    })?;

    let triples = sem_graph(tx, &op.scope, query, op.skip, op.limit)?;

    Ok(sem_construct::Response { triples })
}

fn sem_describe(tx: &mut StoreTx, op: sem_describe::Op) -> Result<sem_describe::Response, String> {
    let query = sem_query(&op.query, op.base_iri.as_deref(), "DESCRIBE", |kind| {
        matches!(kind, db::semantic::QueryKind::Describe)
    })?;

    let triples = sem_graph(tx, &op.scope, query, op.skip, op.limit)?;

    Ok(sem_describe::Response { triples })
}

fn sem_graph(
    tx: &mut StoreTx,
    scope: &Scope,
    query: db::semantic::Query,
    skip: Option<usize>,
    limit: Option<usize>,
) -> Result<Vec<(String, String, String)>, String> {
    let skip = skip.unwrap_or_default();
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);

    tx.sem_graph(key_scope(scope), query, skip, limit)
        .map_err(|err| format_error(&err))
}
