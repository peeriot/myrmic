use crate::domain;
use crate::domain::{
    Blob, BlobId, BlobRef, Entity, Fields, Hash, Id, IdRef, Key, Measurement, MeasurementBody,
    Meta, Path, RawKey, Scope, SyncPoint, Table, Tags, Timestamp, UserKey, Value, ValueRef, api,
};
use crate::semantic::{Query, Update};
use anyhow::Context as _;
use db_commons::models::replication::{SyncMarker, SyncMeta};
use db_commons::models::{SyncPointId, TbOrderBy, TsOrderBy};
use fjall::{Readable, Slice};
use sha2::Digest;
use skey::StoreKey;
use spareval::QueryResults;
use std::collections::HashSet;
use std::ops::{Bound, RangeBounds};
use std::time::Duration;

mod semantic;

pub struct Transaction<M = ()> {
    ks: fjall::OptimisticTxKeyspace,
    tx: fjall::OptimisticWriteTx,
    ts: uhlc::Timestamp,
    active_scopes: HashSet<api::Scope>,
    retention_period: Option<Duration>,
    idle_timeout: Option<Duration>,
    last_used: std::time::Instant,
    metadata: Option<M>,
}

#[derive(skey::StoreKey)]
#[repr(u32)]
/// Because the keys are ordered, it makes sense that certain types take priority over others.
/// ie, if there's an insert a N, then a delete of N should be seen first (so we know we can ignore the insert).
/// If there's a need to "undo" a delete, then simply _delete_ the delete.
/// The gaps are just leaving room in case other kinds need to be added.
enum KeyKind {
    Deletion = 5,
    Insertion = 10,
}

fn encode(user_key: &mut Vec<u8>, ts: u64, kind: KeyKind) {
    let ts = u64::MAX - ts;

    user_key.extend_from_slice(&ts.to_be_bytes());
    user_key.extend_from_slice(&(kind as u32).to_be_bytes());
}

/// Tag byte for the version-index region. Data keys start with the scope
/// alias's `@`, sync points with `#`; `^` keeps the index disjoint from both.
const VERSION_INDEX_TAG: u8 = b'^';

/// Flag row marking the version index as built, so opening an older database
/// triggers a one-time backfill.
pub(crate) const VERSION_INDEX_FLAG: &[u8] = b"!vidx1";

/// Index row key: `^ ‖ ts.be ‖ <full data key>` (empty value). Every data row
/// of a version lands in one contiguous range, so per-version operations
/// (changeset serving, chunk deletion) need not scan the whole scope.
fn version_index_key(ts: u64, data_key: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + size_of::<u64>() + data_key.len());
    key.push(VERSION_INDEX_TAG);
    key.extend_from_slice(&ts.to_be_bytes());
    key.extend_from_slice(data_key);
    key
}

/// Prefix covering every index row of `scope` at exactly `ts` (data keys start
/// with their scope's encoding).
fn version_index_prefix(ts: u64, scope_raw: &[u8]) -> Vec<u8> {
    version_index_key(ts, scope_raw)
}

/// The data key an index row points at.
fn version_index_data_key(index_key: &[u8]) -> anyhow::Result<&[u8]> {
    index_key
        .get(1 + size_of::<u64>()..)
        .context("version index key too short")
}

fn decode(data: &[u8]) -> anyhow::Result<(&[u8], u64, KeyKind)> {
    // `data` is a raw key read back from the store, so its length is untrusted; guard
    // the split offsets against underflow before slicing (a short/corrupt key would
    // otherwise panic inside `split_at`).
    const KIND_LEN: usize = size_of::<u32>();
    const TS_LEN: usize = size_of::<u64>();

    let kind_at = data
        .len()
        .checked_sub(KIND_LEN)
        .context("key too short to contain a kind tag")?;
    let (data, kind) = data.split_at(kind_at);
    let kind: KeyKind = skey::StoreKey::decode_from_bytes(kind).context("unable to decode kind")?;

    let ts_at = data
        .len()
        .checked_sub(TS_LEN)
        .context("key too short to contain a timestamp")?;
    let (data, ts) = data.split_at(ts_at);
    let ts: u64 = skey::StoreKey::decode_from_bytes(ts).context("unable to decode timestamp")?;
    let ts = u64::MAX - ts;

    Ok((data, ts, kind))
}

impl<M> Transaction<M> {
    pub fn start(
        ks: fjall::OptimisticTxKeyspace,
        tx: fjall::OptimisticWriteTx,
        ts: uhlc::Timestamp,
        opts: &crate::store::TransactionOptions,
    ) -> Self {
        Self {
            ks,
            tx,
            ts,
            active_scopes: Default::default(),
            retention_period: opts.retention_period,
            idle_timeout: opts.idle_timeout,
            last_used: std::time::Instant::now(),
            metadata: None,
        }
    }

    pub fn metadata(&self) -> Option<&M> {
        self.metadata.as_ref()
    }

    pub fn metadata_or_default(&mut self) -> &mut M
    where
        M: Default,
    {
        self.metadata.get_or_insert_with(M::default)
    }

    pub fn take_metadata(&mut self) -> Option<M> {
        self.metadata.take()
    }

    /// Marks the transaction as used, pushing back the idle deadline.
    pub(crate) fn touch(&mut self) {
        self.last_used = std::time::Instant::now();
    }

    pub(crate) fn idle_expired(&self, now: std::time::Instant) -> bool {
        self.idle_timeout
            .is_some_and(|timeout| now.duration_since(self.last_used) >= timeout)
    }

    fn split_ts(&self) -> (domain::Version, domain::NodeId) {
        let ts = self.ts;
        let id = ts.get_id().to_le_bytes();
        let ts = ts.get_time().as_u64();
        (ts, id)
    }

    pub fn timestamp(&self) -> uhlc::Timestamp {
        self.ts
    }

    /// Scopes this transaction has written to so far.
    pub fn touched_scopes(&self) -> impl Iterator<Item = &api::Scope> {
        self.active_scopes.iter()
    }

    fn write_scope(&mut self, namespace: &str, database: &str, schema: &str) -> u64 {
        let n_scope = api::Scope {
            namespace: namespace.to_owned(),
            database: database.to_owned(),
            schema: schema.to_owned(),
        };

        self.active_scopes.insert(n_scope);

        self.ts.get_time().as_u64()
    }
}

/// The lowest level operations.
/// These don't make assumptions about the keys, it just aids inserting/deleting them.
impl<M> Transaction<M> {
    fn take_raw(&mut self, key: &[u8]) -> anyhow::Result<Option<Slice>> {
        Ok(self.tx.take(&self.ks, key)?)
    }

    fn delete_raw(&mut self, key: &[u8]) {
        self.tx.remove(&self.ks, key);
    }

    fn insert_raw(&mut self, key: &[u8], value: &[u8]) {
        self.tx.insert(&self.ks, key, value);
    }

    fn insert<'a, S, V>(&mut self, key: &S, value: &V) -> anyhow::Result<()>
    where
        S: skey::StoreKey<'a>,
        V: serde::Serialize,
    {
        let key = key.encode().context("unable to encode key")?;
        let value = postcard::to_allocvec(value).context("unable to serialise value")?;

        self.insert_raw(&key, &value);

        Ok(())
    }

    fn range_of<K: AsRef<[u8]>, R: RangeBounds<K>>(&self, range: R) -> fjall::Iter {
        self.tx.range(&self.ks, range)
    }

    fn range_of_untracked<K: AsRef<[u8]>, R: RangeBounds<K>>(&self, range: R) -> fjall::Iter {
        self.tx.untrack().range(&self.ks, range)
    }

    fn prefix_of(&self, key: &[u8]) -> fjall::Iter {
        self.tx.prefix(&self.ks, key)
    }

    fn prefix_of_untracked(&self, key: &[u8]) -> fjall::Iter {
        self.tx.untrack().prefix(&self.ks, key)
    }

    // fn find_first(&self, key: &[u8]) -> Option<fjall::Guard> {
    //     self.prefix_of(key).next()
    // }

    // fn find_first_key(&self, key: &[u8]) -> anyhow::Result<Option<Slice>> {
    //     Ok(self.find_first(key).map(fjall::Guard::key).transpose()?)
    // }

    // fn find_first_untracked(&self, key: &[u8]) -> Option<fjall::Guard> {
    //     self.prefix_of_untracked(key).next()
    // }

    // fn find_first_key_untracked_raw(&self, key: &[u8]) -> anyhow::Result<Option<Slice>> {
    //     Ok(self
    //         .find_first_untracked(key)
    //         .map(fjall::Guard::key)
    //         .transpose()?)
    // }

    // fn find_first_key_untracked<'a, S>(&self, key: &S) -> anyhow::Result<Option<Slice>>
    // where
    //     S: skey::StoreKey<'a>,
    // {
    //     let key = key.encode()?;
    //     self.find_first_key_untracked_raw(&key)
    // }

    fn find_last_untracked(&self, key: &[u8]) -> Option<fjall::Guard> {
        self.prefix_of_untracked(key).next_back()
    }

    fn find_last_key_untracked_raw(&self, key: &[u8]) -> anyhow::Result<Option<Slice>> {
        Ok(self
            .find_last_untracked(key)
            .map(fjall::Guard::key)
            .transpose()?)
    }

    fn find_last_key_untracked<'a, S>(&self, key: &S) -> anyhow::Result<Option<Slice>>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.find_last_key_untracked_raw(&key)
    }

    fn contains_key_raw_untracked(&self, key: &[u8]) -> bool {
        self.prefix_of_untracked(key).next().is_some()
    }

    fn contains_key_untracked<'a, S>(&self, key: &S) -> anyhow::Result<bool>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        Ok(self.contains_key_raw_untracked(&key))
    }
}

/// Mid-level operations.
/// This contains a lot of the basic building blocks, and does make assumptions about the keys.
/// These all assume that the key in question will be encoded with the timestamp.
/// Some types, like the sync-point, aren't encoded via time (at least, not this way, they have their own concept of time encoding)
/// Because of that, this should only be used with the base api types, kv, table, etc
impl<M> Transaction<M> {
    fn put_if_absent_raw(&mut self, key: Vec<u8>, value: &[u8]) -> anyhow::Result<bool> {
        let (ts, _me) = self.split_ts();
        if self.get_from_raw(&key, ts)?.is_some() {
            Ok(false)
        } else {
            self.put_at_raw(key, value, ts)?;
            Ok(true)
        }
    }

    fn put_if_absent<'a, S>(&mut self, key: &S, value: &[u8]) -> anyhow::Result<bool>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.put_if_absent_raw(key, value)
    }

    fn put_raw(&mut self, key: Vec<u8>, value: &[u8]) -> anyhow::Result<()> {
        let (ts, _me) = self.split_ts();
        self.put_at_raw(key, value, ts)?;
        Ok(())
    }

    fn put<'a, S>(&mut self, key: &S, value: &[u8]) -> anyhow::Result<()>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.put_raw(key, value)?;
        Ok(())
    }

    fn put_at_raw(&mut self, mut key: Vec<u8>, value: &[u8], ts: u64) -> anyhow::Result<()> {
        let mark = key.len();
        {
            encode(&mut key, ts, KeyKind::Deletion);
            let _drop = self.take_raw(key.as_slice())?;
            self.delete_raw(&version_index_key(ts, &key));
        }
        key.truncate(mark);

        self.insert_at_raw(key, value, ts);
        Ok(())
    }

    /// Bypasses lookup check, only used by replication
    fn insert_at_raw(&mut self, mut key: Vec<u8>, value: &[u8], ts: u64) {
        encode(&mut key, ts, KeyKind::Insertion);
        self.insert_raw(&version_index_key(ts, &key), b"");
        self.insert_raw(key.as_slice(), value);
    }

    fn put_at<'a, S>(&mut self, key: &S, value: &[u8], ts: u64) -> anyhow::Result<()>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.put_at_raw(key, value, ts)?;
        Ok(())
    }

    fn erase_raw(&mut self, key: Vec<u8>) {
        let (ts, _me) = self.split_ts();
        self.erase_at_raw(key, ts);
    }

    fn erase<'a, S>(&mut self, key: &S) -> anyhow::Result<()>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.erase_raw(key);
        Ok(())
    }

    fn erase_at_raw(&mut self, mut key: Vec<u8>, ts: u64) {
        encode(&mut key, ts, KeyKind::Deletion);
        self.insert_raw(&version_index_key(ts, &key), b"");
        self.insert_raw(key.as_slice(), b"");
    }

    fn erase_at<'a, S>(&mut self, key: &S, ts: u64) -> anyhow::Result<()>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.erase_at_raw(key, ts);
        Ok(())
    }

    #[expect(
        unused,
        reason = "kept as a symmetric counterpart to get_from_raw; not yet called"
    )]
    fn get_raw(&self, key: &[u8]) -> anyhow::Result<Option<(Slice, Slice)>> {
        self.get_from_raw(key, u64::MAX)
    }

    fn get<'a, S>(&self, key: &S) -> anyhow::Result<Option<(Slice, Slice)>>
    where
        S: skey::StoreKey<'a>,
    {
        self.get_from(key, u64::MAX)
    }

    fn get_from_raw(&self, key: &[u8], ts: u64) -> anyhow::Result<Option<(Slice, Slice)>> {
        let it = self.prefix_of(key);

        for guard in it {
            let (k, v) = guard.into_inner()?;
            let (user_key, uk_ts, kind) = decode(&k)?;

            // We're using a prefix scan because, obviously, we're extending the "user key" with our own stuff.
            // But this means that if there's a partial overlap in keys we use (ie, blob meta and blob path)
            // Then this matches both.
            // So we just need to double check we're _actually_ resolving the correct key.
            if user_key != key {
                continue;
            }

            // skipping past the keys that are above the requested time range.
            if uk_ts > ts {
                continue;
            }

            return match kind {
                KeyKind::Deletion => Ok(None),
                KeyKind::Insertion => Ok(Some((Slice::new(user_key), v))),
            };
        }

        Ok(None)
    }

    fn get_from<'a, S>(&self, key: &S, ts: u64) -> anyhow::Result<Option<(Slice, Slice)>>
    where
        S: skey::StoreKey<'a>,
    {
        let key = key.encode()?;
        self.get_from_raw(&key, ts)
    }

    fn prefix_latest(&self, key: &[u8]) -> impl Iterator<Item = anyhow::Result<Slice>> {
        // versions sort contiguously, so collapse adjacent equal keys.
        let mut prev: Option<Vec<u8>> = None;

        self.prefix_of(key).filter_map(move |g| {
            let k = match g.key().context("unable to read entry") {
                Ok(k) => k,
                Err(err) => {
                    return Some(Err(err));
                }
            };
            let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };

            if prev.as_deref() == Some(user_key) {
                return None;
            }
            match &mut prev {
                Some(p) => {
                    p.clear();
                    p.extend_from_slice(user_key);
                }
                None => prev = Some(user_key.to_vec()),
            }

            match kind {
                KeyKind::Deletion => None,
                KeyKind::Insertion => Some(anyhow::Ok(Slice::new(user_key))),
            }
        })
    }

    fn prefix_latest_untracked(&self, key: &[u8]) -> impl Iterator<Item = anyhow::Result<Slice>> {
        // versions sort contiguously, so collapse adjacent equal keys.
        let mut prev: Option<Vec<u8>> = None;

        self.prefix_of_untracked(key).filter_map(move |g| {
            let k = match g.key().context("unable to read entry") {
                Ok(k) => k,
                Err(err) => {
                    return Some(Err(err));
                }
            };
            let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };

            if prev.as_deref() == Some(user_key) {
                return None;
            }
            match &mut prev {
                Some(p) => {
                    p.clear();
                    p.extend_from_slice(user_key);
                }
                None => prev = Some(user_key.to_vec()),
            }

            match kind {
                KeyKind::Deletion => None,
                KeyKind::Insertion => Some(anyhow::Ok(Slice::new(user_key))),
            }
        })
    }

    fn range_latest(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
    ) -> impl Iterator<Item = anyhow::Result<Slice>> {
        // versions sort contiguously, so collapse adjacent equal keys.
        let mut prev: Option<Vec<u8>> = None;

        self.range_of(lower..upper).filter_map(move |g| {
            let k = match g.key().context("unable to read entry") {
                Ok(k) => k,
                Err(err) => {
                    return Some(Err(err));
                }
            };
            let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };

            if prev.as_deref() == Some(user_key) {
                return None;
            }
            match &mut prev {
                Some(p) => {
                    p.clear();
                    p.extend_from_slice(user_key);
                }
                None => prev = Some(user_key.to_vec()),
            }

            match kind {
                KeyKind::Deletion => None,
                KeyKind::Insertion => Some(anyhow::Ok(Slice::new(user_key))),
            }
        })
    }

    fn range_latest_kv(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
    ) -> impl Iterator<Item = anyhow::Result<(Slice, Slice)>> {
        // versions sort contiguously, so collapse adjacent equal keys.
        let mut prev: Option<Vec<u8>> = None;

        self.range_of(lower..upper).filter_map(move |g| {
            let (k, v) = match g.into_inner().context("unable to read entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };
            let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };

            if prev.as_deref() == Some(user_key) {
                return None;
            }
            match &mut prev {
                Some(p) => {
                    p.clear();
                    p.extend_from_slice(user_key);
                }
                None => prev = Some(user_key.to_vec()),
            }

            match kind {
                KeyKind::Deletion => None,
                KeyKind::Insertion => Some(anyhow::Ok((Slice::new(user_key), v))),
            }
        })
    }

    fn range_latest_kv_untracked(
        &self,
        lower: Vec<u8>,
        upper: Option<Vec<u8>>,
    ) -> impl Iterator<Item = anyhow::Result<(Slice, Slice)>> {
        // versions sort contiguously, so collapse adjacent equal keys.
        let mut prev: Option<Vec<u8>> = None;

        let range = (
            Bound::Included(lower),
            upper.map_or(Bound::Unbounded, Bound::Excluded),
        );

        self.range_of_untracked(range).filter_map(move |g| {
            let (k, v) = match g.into_inner().context("unable to read entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };
            let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                Ok(kv) => kv,
                Err(err) => {
                    return Some(Err(err));
                }
            };

            if prev.as_deref() == Some(user_key) {
                return None;
            }
            match &mut prev {
                Some(p) => {
                    p.clear();
                    p.extend_from_slice(user_key);
                }
                None => prev = Some(user_key.to_vec()),
            }

            match kind {
                KeyKind::Deletion => None,
                KeyKind::Insertion => Some(anyhow::Ok((Slice::new(user_key), v))),
            }
        })
    }

    fn range_latest_kv_untracked_rev(
        &self,
        lower: Vec<u8>,
        upper: Option<Vec<u8>>,
    ) -> impl Iterator<Item = anyhow::Result<(Slice, Slice)>> {
        let range = (
            Bound::Included(lower),
            upper.map_or(Bound::Unbounded, Bound::Excluded),
        );

        let mut it = self.range_of_untracked(range).rev();
        let mut pending: Option<(Vec<u8>, Slice, KeyKind)> = None;
        let mut done = false;

        core::iter::from_fn(move || {
            if done {
                return None;
            }
            loop {
                let Some(g) = it.next() else {
                    done = true;
                    return match pending.take() {
                        Some((k, v, KeyKind::Insertion)) => Some(anyhow::Ok((Slice::new(&k), v))),
                        _ => None,
                    };
                };
                let (k, v) = match g.into_inner().context("unable to read entry") {
                    Ok(kv) => kv,
                    Err(err) => return Some(Err(err)),
                };
                let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                    Ok(kv) => kv,
                    Err(err) => return Some(Err(err)),
                };

                if matches!(&pending, Some((p, _, _)) if p.as_slice() == user_key) {
                    pending = Some((user_key.to_vec(), v, kind));
                } else if let Some((fk, fv, KeyKind::Insertion)) =
                    pending.replace((user_key.to_vec(), v, kind))
                {
                    return Some(anyhow::Ok((Slice::new(&fk), fv)));
                }
            }
        })
    }

    fn range_latest_kv_rev(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
    ) -> impl Iterator<Item = anyhow::Result<(Slice, Slice)>> {
        let mut it = self.range_of(lower..upper).rev();
        let mut pending: Option<(Vec<u8>, Slice, KeyKind)> = None;
        let mut done = false;

        core::iter::from_fn(move || {
            if done {
                return None;
            }
            loop {
                let Some(g) = it.next() else {
                    done = true;
                    return match pending.take() {
                        Some((k, v, KeyKind::Insertion)) => Some(anyhow::Ok((Slice::new(&k), v))),
                        _ => None,
                    };
                };
                let (k, v) = match g.into_inner().context("unable to read entry") {
                    Ok(kv) => kv,
                    Err(err) => return Some(Err(err)),
                };
                let (user_key, _ts, kind) = match decode(&k).context("unable to decode entry") {
                    Ok(kv) => kv,
                    Err(err) => return Some(Err(err)),
                };

                if matches!(&pending, Some((p, _, _)) if p.as_slice() == user_key) {
                    pending = Some((user_key.to_vec(), v, kind));
                } else if let Some((fk, fv, KeyKind::Insertion)) =
                    pending.replace((user_key.to_vec(), v, kind))
                {
                    return Some(anyhow::Ok((Slice::new(&fk), fv)));
                }
            }
        })
    }
}

/// These are the high level operations.
/// The only difference between this block, and the next is these aren't placed on the API.
/// Most of them work with the api types, but focus on doing book-keeping, etc.
impl<M> Transaction<M> {
    /// This searches for the last syncpoint for a given scope.
    /// It does so _untracked_, meaning there will be _no_ conflicts detected for the search.
    ///
    /// If the version is provided, it searches from that point.
    pub(crate) fn find_last_syncpoint_untracked<'a>(
        &mut self,
        scope: Scope<'a>,
        ts: Option<domain::Version>,
    ) -> anyhow::Result<Option<SyncPoint<'a>>> {
        let sp = Key::sync_point()
            .namespace(scope.namespace)
            .database(scope.database)
            .schema(scope.schema);

        let last = if let Some(ts) = ts {
            self.find_last_key_untracked(&sp.ts(ts))?
        } else {
            self.find_last_key_untracked(&sp)?
        }
        .map(|key| SyncPoint::decode_from_bytes(&key).map(|s| s.as_id()))
        .transpose()?;

        let Some(last) = last else {
            return Ok(None);
        };

        Ok(Some(sp.with_sp_id(last)))
    }

    /// Deletes every data row of `scope` at exactly `ts` (and its index rows),
    /// returning how many data rows were removed.
    fn delete_version_rows(
        &mut self,
        scope_raw: &[u8],
        ts: domain::Version,
    ) -> anyhow::Result<usize> {
        let prefix = version_index_prefix(ts, scope_raw);

        let mut index_keys = vec![];
        for guard in self.prefix_of(&prefix) {
            index_keys.push(guard.key().context("unable to read version index")?);
        }

        let count = index_keys.len();
        for ikey in index_keys {
            self.delete_raw(version_index_data_key(&ikey)?);
            self.delete_raw(&ikey);
        }

        Ok(count)
    }

    fn delete_scope_at_version(
        &mut self,
        scope: Scope<'_>,
        ts: domain::Version,
    ) -> anyhow::Result<()> {
        let raw = scope.encode()?;

        let count = self.delete_version_rows(&raw, ts)?;

        tracing::debug!("deleted {} entries", count);

        Ok(())
    }

    pub fn collect_latest_heads<F>(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
        mut func: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(api::Scope, SyncPointId, SyncMeta) -> anyhow::Result<()>,
    {
        let mut current_sp: Option<(api::Scope, SyncPointId, SyncMeta)> = None;

        self.find_sync_points_rev(lower, upper, |sp, sm| {
            if let Some((scope, id, _other_sm)) = current_sp.as_ref() {
                // We don't care about older epochs, so if we find more sync points with the same details (sans epoch), then we just skip it.
                let same = id.1 == sp.ts
                    && scope.namespace == sp.namespace
                    && scope.database == sp.database
                    && scope.schema == sp.schema;
                if same {
                    return Ok(());
                }
            }

            if let Some((scope, id, sm)) = current_sp.take() {
                func(scope, id, sm)?;
            }

            let scope = api::Scope::new(sp.namespace, sp.database, sp.schema);

            current_sp = Some((scope, sp.as_id(), sm));

            Ok(())
        })?;

        if let Some((scope, id, sm)) = current_sp.take() {
            func(scope, id, sm)?;
        }

        Ok(())
    }

    pub(crate) fn find_sync_points<F>(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
        mut func: F,
    ) -> anyhow::Result<()>
    where
        F: for<'a> FnMut(SyncPoint<'a>, SyncMeta) -> anyhow::Result<()>,
    {
        let it = self.range_of(lower..upper);

        for guard in it {
            let (k, v) = guard.into_inner().context("unable to read data")?;

            let sp = SyncPoint::decode_from_bytes(&k)
                .context("unable to decode sync point from user key")?;
            let sm = postcard::from_bytes(&v).context("unable to deser meta")?;

            func(sp, sm).context("user defined func error")?;
        }

        Ok(())
    }

    /// Like [`Self::find_sync_points`], stopping early once `func` returns
    /// `false`.
    pub(crate) fn find_sync_points_while<F>(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
        mut func: F,
    ) -> anyhow::Result<()>
    where
        F: for<'a> FnMut(SyncPoint<'a>, SyncMeta) -> anyhow::Result<bool>,
    {
        for guard in self.range_of(lower..upper) {
            let (k, v) = guard.into_inner().context("unable to read data")?;

            let sp = SyncPoint::decode_from_bytes(&k)
                .context("unable to decode sync point from user key")?;
            let sm = postcard::from_bytes(&v).context("unable to deser meta")?;

            if !func(sp, sm).context("user defined func error")? {
                break;
            }
        }

        Ok(())
    }

    pub(crate) fn find_sync_points_rev<F>(
        &self,
        lower: Vec<u8>,
        upper: Vec<u8>,
        mut func: F,
    ) -> anyhow::Result<()>
    where
        F: for<'a> FnMut(SyncPoint<'a>, SyncMeta) -> anyhow::Result<()>,
    {
        let it = self.range_of(lower..upper).rev();

        for guard in it {
            let (k, v) = guard.into_inner().context("unable to read data")?;

            let sp = SyncPoint::decode_from_bytes(&k)?;
            let sm = postcard::from_bytes(&v)?;

            func(sp, sm)?;
        }

        Ok(())
    }

    /// Whether the version index has been built for this database.
    pub(crate) fn version_index_ready(&self) -> anyhow::Result<bool> {
        Ok(self.tx.get(&self.ks, VERSION_INDEX_FLAG)?.is_some())
    }

    pub(crate) fn set_version_index_ready(&mut self) {
        self.insert_raw(VERSION_INDEX_FLAG, b"");
    }

    /// Whether the store holds no data or sync-point rows at all. Data keys
    /// start with the scope alias's `@`, sync points with `#`.
    pub(crate) fn holds_no_user_data(&self) -> bool {
        let empty = |tag: u8| {
            self.range_of_untracked([tag].as_slice()..[tag + 1].as_slice())
                .next()
                .is_none()
        };

        empty(b'@') && empty(b'#')
    }

    #[cfg(test)]
    pub(crate) fn strip_version_index_for_test(&mut self) -> anyhow::Result<()> {
        let mut keys = vec![];
        for guard in self.prefix_of(&[VERSION_INDEX_TAG]) {
            keys.push(guard.key().context("unable to read version index")?);
        }
        for key in keys {
            self.delete_raw(&key);
        }
        self.delete_raw(VERSION_INDEX_FLAG);
        Ok(())
    }

    /// Re-marks a purged sync point as a deletion: the head stays announced,
    /// peers see an explicitly empty version, and GC never re-collects it.
    pub(crate) fn mark_syncpoint_purged(
        &mut self,
        sp: &SyncPoint<'_>,
        sm: SyncMeta,
    ) -> anyhow::Result<()> {
        let meta = SyncMeta {
            marker: SyncMarker::Deletion,
            ..sm
        };
        self.insert(sp, &meta)
    }

    pub fn delete_chunk(&mut self, sp: SyncPoint<'_>) -> anyhow::Result<()> {
        let scope = Key::new_scope(sp.namespace, sp.database, sp.schema);
        let raw = scope.encode().context("unable to encode scope")?;

        self.delete_version_rows(&raw, sp.ts)?;

        Ok(())
    }

    /// Forgets a sync point outright: its rows *and* the point itself, leaving
    /// no marker behind.
    ///
    /// Deliberately not [`Self::mark_syncpoint_purged`]. A
    /// [`SyncMarker::Deletion`] marker is a *replicated instruction to erase*
    /// the version — [`Self::insert_changeset`] acts on one by calling
    /// `delete_scope_at_version` — which is only sound when every replica
    /// reaches the same verdict independently, as retention expiry does because
    /// retention travels with each version. A node releasing data it has
    /// offloaded is deciding unilaterally, so asserting `Deletion` would tell
    /// the holders that still need the version to drop it. Forgetting asserts
    /// nothing: this node's head simply falls back to what it still holds.
    pub(crate) fn forget_chunk(&mut self, sp: SyncPoint<'_>) -> anyhow::Result<()> {
        let key = sp.encode().context("unable to encode sync point")?;

        self.delete_chunk(sp)?;
        self.delete_raw(&key);

        Ok(())
    }

    pub fn changeset_for(&self, sp: SyncPoint<'_>) -> anyhow::Result<Vec<(RawKey, Option<Value>)>> {
        let scope = Key::new_scope(sp.namespace, sp.database, sp.schema);
        let raw = scope.encode().context("unable to encode scope")?;

        let prefix = version_index_prefix(sp.ts, &raw);

        let mut out = vec![];
        // One marker per key: deletion sorts first, so the first wins.
        let mut prev: Option<Vec<u8>> = None;

        for guard in self.prefix_of(&prefix) {
            let ikey = guard.key().context("unable to read version index")?;
            let data_key = version_index_data_key(&ikey)?;
            let (user_key, _ts, kind) = decode(data_key)?;

            if prev.as_deref() == Some(user_key) {
                continue;
            }
            prev = Some(user_key.to_vec());

            let value = match kind {
                KeyKind::Insertion => {
                    let value = self
                        .tx
                        .get(&self.ks, data_key)
                        .context("unable to read data")?
                        .context("version index points at a missing data row")?;
                    Some(value.to_vec())
                }
                KeyKind::Deletion => None,
            };

            out.push((user_key.to_vec(), value));
        }

        Ok(out)
    }

    pub fn insert_changeset(
        &mut self,
        sp: SyncPoint<'_>,
        sm: SyncMeta,
        entries: &[(RawKey, Option<Value>)],
    ) -> anyhow::Result<()> {
        if self.contains_key_untracked(&sp)? {
            tracing::debug!(
                "Already Sync'd {}/{}/{} @ {}",
                sp.namespace,
                sp.database,
                sp.schema,
                sp.ts,
            );
            return Ok(());
        }

        self.insert(&sp, &sm)?;

        let ts = sp.ts;

        // A deletion marker rolls back its whole version: erase our data at that
        // timestamp so reads fall through to any older version we still hold.
        if matches!(sm.marker, SyncMarker::Deletion) {
            let scope = Key::new_scope(sp.namespace, sp.database, sp.schema);

            self.delete_scope_at_version(scope, ts)?;
            return Ok(());
        }

        tracing::debug!("inserting {} entries", entries.len());

        for (key, value) in entries {
            if let Some(value) = value {
                self.insert_at_raw(key.clone(), value, ts);
            } else {
                self.erase_at_raw(key.clone(), ts);
            }
        }

        Ok(())
    }
}

/// This is the API block.
/// All of the operations here should be exposed (to some degree) through the db-client.
/// You could think of this as the highest level.
impl<M> Transaction<M> {
    pub fn commit(mut self) -> anyhow::Result<()> {
        let (ts, id) = self.split_ts();

        let active_scopes = std::mem::take(&mut self.active_scopes);

        for scope in active_scopes {
            let scope = Key::new_scope(&scope.namespace, &scope.database, &scope.schema);

            let last = self
                .find_last_syncpoint_untracked(scope, None)?
                .map(|sp| sp.as_id());

            let meta = SyncMeta {
                parent: last,
                parent_epoch: None,
                marker: SyncMarker::Mutation,
                retention_period: self.retention_period,
            };

            let sp = Key::sync_point().scope(scope).ts(ts).epoch(ts).id(id);

            tracing::debug!(
                "Commit {}/{}/{} @ {}/{}",
                sp.namespace,
                sp.database,
                sp.schema,
                sp.ts,
                sp.epoch,
            );
            if let Some(parent) = meta.parent {
                tracing::debug!("\tparent: {:?}", parent);
            }

            self.insert(&sp, &meta)?;
        }

        self.tx.commit()?.map_err(|_err| {
            anyhow::anyhow!("Transaction encountered concurrent changes and wasn't applied.")
        })?;
        Ok(())
    }

    pub fn rollback(self) {
        self.tx.rollback();
    }

    pub fn take_snapshot(&self, scope: Scope<'_>) -> anyhow::Result<domain::Snapshot> {
        let (lower, upper) = Key::sync_point()
            .scope(scope)
            .range()
            .context("unable to calculate range")?;

        let mut chunks = vec![];
        self.find_sync_points(lower, upper, |sp, meta| {
            let id = (sp.epoch, sp.ts, sp.id);
            let entries = match meta.marker {
                SyncMarker::Deletion => vec![],
                SyncMarker::Mutation => {
                    self.changeset_for(sp).context("unable to load changeset")?
                }
            };

            chunks.push(domain::Chunk { id, meta, entries });
            Ok(())
        })
        .context("unable to create snapshot")?;

        Ok(chunks)
    }

    pub fn restore_snapshot(
        &mut self,
        scope: Scope<'_>,
        snapshot: domain::Snapshot,
    ) -> anyhow::Result<()> {
        // If we are restoring the data, there's two possible reasons:
        //  * One or more of the snapshots that _are_ in the db aren't "correct".
        //  * For whatever reason, the original data (or syncpoint) isn't present.
        //
        // So, with that in mind, the process for restoring is fairly straight-forward.
        // We delete everything that came before (sync-point wise).
        // Then we can insert our own syncpoints.
        // This is what we call an authorative restore.
        // There's still other types we can add, and will probably do in time.
        //
        // Each sync-point keeps its original version (its logical time), but is
        // re-stamped with the current epoch. Replication ranks sync-points
        // per-version by epoch, so bumping the epoch is what tells peers the
        // version changed and makes them re-pull the restored state instead of
        // re-pushing whatever they still hold, as that's the whole point of the
        // epoch. The removals the restore implies are expressed as one fresh
        // soft-delete changeset, the same primitive a normal delete replicates as.

        let (now, me) = self.split_ts();

        let scope = Key::new_scope(scope.namespace, scope.database, scope.schema);
        let sp = Key::sync_point().scope(scope);

        tracing::debug!("applying snapshot with {} chunk(s)", snapshot.len());

        // Bounded to this scope's sync-point range: the sweep must never run
        // into a neighbouring scope's rows.
        let (lower, upper) = sp.range().context("unable to construct sync point range")?;

        let iter = if let Some(parent) = snapshot.first().and_then(|p| p.meta.parent) {
            let start = Key::sync_point().scope(scope).with_sp_id(parent).encode()?;
            self.range_of(start..upper).skip(1)
        } else {
            #[expect(
                clippy::iter_skip_zero,
                reason = "skip(0) mirrors the sibling branch's skip(1) so both arms share a type"
            )]
            self.range_of(lower..upper).skip(0)
        };

        let mut deleted = vec![];

        for id in iter {
            let key = id.key()?;
            let sp = SyncPoint::decode_from_bytes(&key)?;

            tracing::debug!("deleting {} @ {}", sp.ts, sp.epoch);

            self.delete_scope_at_version(scope, sp.ts)?;

            deleted.push(sp.as_id());

            let sp = sp.encode()?;
            self.delete_raw(&sp);
        }

        // Re-insert each snapshot chunk at its original version, re-stamped with the
        // current epoch. The higher epoch makes peers re-pull the version.
        let epoch = now;

        for chunk in snapshot {
            // @TODO jezza - 15 June 2026: Double check that links up properly.
            let parent_epoch = deleted
                .iter()
                .position(|id| *id == chunk.id)
                .map(|pos| deleted.remove(pos))
                .map(|id| id.0);

            let domain::Chunk {
                id: (_epoch, version, _node_id),
                meta,
                entries,
            } = chunk;

            let last = self
                .find_last_syncpoint_untracked(scope, Some(version))?
                .map(|s| s.as_id());

            let meta = SyncMeta {
                parent: last,
                parent_epoch,
                marker: meta.marker,
                retention_period: meta.retention_period,
            };

            let point = Key::new_sync_point(
                scope.namespace,
                scope.database,
                scope.schema,
                version,
                epoch,
                me,
            );

            self.insert_changeset(point, meta, &entries)?;
        }

        for id in deleted {
            let (epoch, version, _id) = id;

            let last = self
                .find_last_syncpoint_untracked(scope, Some(version))?
                .map(|s| s.as_id());

            let meta = SyncMeta {
                parent: last,
                parent_epoch: Some(epoch),
                marker: SyncMarker::Deletion,
                retention_period: None,
            };

            let point = Key::new_sync_point(
                scope.namespace,
                scope.database,
                scope.schema,
                version,
                now,
                me,
            );

            self.insert(&point, &meta)?;
        }

        Ok(())
    }

    pub fn key_get(&mut self, key: UserKey<'_>) -> anyhow::Result<Option<Value>> {
        self.get(&key).map(|s| s.map(|(_k, v)| v.to_vec()))
    }

    pub fn key_put(&mut self, key: UserKey<'_>, value: ValueRef<'_>) -> anyhow::Result<()> {
        let ts = self.write_scope(key.namespace, key.database, key.schema);

        self.put_at(&key, value, ts)?;

        Ok(())
    }

    pub fn key_delete(&mut self, key: UserKey<'_>) -> anyhow::Result<()> {
        let ts = self.write_scope(key.namespace, key.database, key.schema);

        self.erase_at(&key, ts)?;

        Ok(())
    }

    pub fn key_prefix(&mut self, scope: Scope<'_>, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut key = scope.only_kv().key(prefix).encode()?;

        // We want to remove the null encoding
        key.pop();

        let mut out = vec![];

        let iter = self.prefix_of_untracked(&key);
        for g in iter {
            let key = g.key()?;
            let (user_key, _ts, kind) = decode(&key)?;
            match kind {
                KeyKind::Insertion => (),
                KeyKind::Deletion => {
                    continue;
                }
            }

            let user_key = UserKey::decode_from_bytes(user_key)?;

            out.push(String::from(user_key.key));
        }

        Ok(out)
    }

    pub fn tb_count(&mut self, table: Table<'_>) -> anyhow::Result<usize> {
        let table = table.encode()?;

        let mut count = 0;

        for key in self.prefix_latest_untracked(&table) {
            let key = key?;
            // Just make sure we're looking at a valid entity.
            let _entity = Entity::decode_from_bytes(&key)?;
            count += 1;
        }

        Ok(count)
    }

    pub fn tb_get(&mut self, table: Table<'_>, id: IdRef<'_>) -> anyhow::Result<Option<Value>> {
        self.get(&table.id(id)).map(|s| s.map(|(_k, v)| v.to_vec()))
    }

    pub fn tb_delete(&mut self, table: Table<'_>, id: IdRef<'_>) -> anyhow::Result<()> {
        let ts = self.write_scope(table.namespace, table.database, table.schema);

        self.erase_at(&table.id(id), ts)?;

        Ok(())
    }

    pub fn tb_insert(
        &mut self,
        table: Table<'_>,
        id: IdRef<'_>,
        value: ValueRef<'_>,
    ) -> anyhow::Result<()> {
        let ts = self.write_scope(table.namespace, table.database, table.schema);

        self.put_at(&table.id(id), value, ts)?;

        Ok(())
    }

    /// Insert many rows into a single table under one write timestamp.
    pub fn tb_insert_batched(
        &mut self,
        table: Table<'_>,
        entries: &[(IdRef<'_>, ValueRef<'_>)],
    ) -> anyhow::Result<()> {
        let ts = self.write_scope(table.namespace, table.database, table.schema);

        for (id, value) in entries {
            self.put_at(&table.id(id), value, ts)?;
        }

        Ok(())
    }

    pub fn tb_list(
        &mut self,
        table: Table<'_>,
        cursor: Option<domain::Cursor>,
        limit: Option<usize>,
        order: Option<TbOrderBy>,
    ) -> anyhow::Result<Vec<(Id, Value)>> {
        let order = order.unwrap_or_default();
        let (table_lo, table_hi) = skey::prefix_to_range(&table.encode()?);

        // The cursor edge is relative to the iteration direction: ascending it bounds the
        // lower (start) edge, descending it bounds the upper (start) edge. `Skip` just drops
        // a fixed count from whichever end iteration begins at.
        let (lower, upper, mut skip) = match (order, cursor) {
            (TbOrderBy::KeyAsc, Some(domain::Cursor::At(id))) => {
                (table.id(id.as_slice()).encode()?, table_hi, 0)
            }
            (TbOrderBy::KeyAsc, Some(domain::Cursor::After(id))) => {
                match skey::prefix_to_range(&table.id(id.as_slice()).encode()?).1 {
                    Some(after) => (after, table_hi, 0),
                    // The id is the maximal key; nothing sorts after it.
                    None => return Ok(vec![]),
                }
            }
            (TbOrderBy::KeyAsc, Some(domain::Cursor::Skip(offset))) => (table_lo, table_hi, offset),
            (TbOrderBy::KeyAsc, None) => (table_lo, table_hi, 0),

            (TbOrderBy::KeyDesc, Some(domain::Cursor::At(id))) => {
                // Include `id` and everything below it.
                match skey::prefix_to_range(&table.id(id.as_slice()).encode()?).1 {
                    Some(after) => (table_lo, Some(after), 0),
                    None => (table_lo, table_hi, 0),
                }
            }
            (TbOrderBy::KeyDesc, Some(domain::Cursor::After(id))) => {
                // Everything strictly below `id`.
                (table_lo, Some(table.id(id.as_slice()).encode()?), 0)
            }
            (TbOrderBy::KeyDesc, Some(domain::Cursor::Skip(offset))) => {
                (table_lo, table_hi, offset)
            }
            (TbOrderBy::KeyDesc, None) => (table_lo, table_hi, 0),
        };

        let limit = limit.unwrap_or(usize::MAX);

        let entries: Box<dyn Iterator<Item = anyhow::Result<(Slice, Slice)>> + '_> = match order {
            TbOrderBy::KeyAsc => Box::new(self.range_latest_kv_untracked(lower, upper)),
            TbOrderBy::KeyDesc => Box::new(self.range_latest_kv_untracked_rev(lower, upper)),
        };

        let mut results = vec![];
        for entry in entries {
            let (key, value) = entry?;

            if skip > 0 {
                skip -= 1;
                continue;
            }
            if results.len() >= limit {
                break;
            }

            let entity = Entity::decode_from_bytes(&key).context("unable to decode row key")?;
            results.push((entity.id.to_vec(), value.to_vec()));
        }

        Ok(results)
    }

    pub fn publish_measurement(
        &mut self,
        scope: Scope<'_>,
        measurement: &str,
        tags: Tags,
        fields: Fields,
        timestamp: Timestamp,
    ) -> anyhow::Result<()> {
        let ts = self.write_scope(scope.namespace, scope.database, scope.schema);

        let key = Key::measurement()
            .namespace(scope.namespace)
            .database(scope.database)
            .schema(scope.schema)
            .measurement(measurement)
            .timestamp(timestamp);

        let body = MeasurementBody { tags, fields };

        let body = postcard::to_allocvec(&body).context("unable to serialise measurement body")?;

        self.put_at(&key, &body, ts)?;

        Ok(())
    }

    pub fn find_measurement(
        &mut self,
        scope: Scope<'_>,
        measurement: &str,
        limit: Option<usize>,
        start: Option<Timestamp>,
        end: Option<Timestamp>,
        order: Option<TsOrderBy>,
    ) -> anyhow::Result<Vec<(Tags, Fields, Timestamp)>> {
        let m = Key::measurement()
            .namespace(scope.namespace)
            .database(scope.database)
            .schema(scope.schema)
            .measurement(measurement);

        // The window is a half-open key interval over the measurement's rows: `start`
        // inclusive, `end` exclusive. A missing bound falls back to the prefix's own
        // range so that side stays open. The measurement timestamp is part of the key,
        // so a bound encoded without a version suffix sits just before every version of
        // that row, giving an inclusive lower and exclusive upper for free.
        let (prefix_lower, prefix_upper) =
            m.range().context("unable to encode measurement prefix")?;

        let lower = match start {
            Some(start) => m
                .timestamp(start)
                .encode()
                .context("unable to encode measurement start")?,
            None => prefix_lower,
        };
        let upper = match end {
            Some(end) => m
                .timestamp(end)
                .encode()
                .context("unable to encode measurement end")?,
            None => prefix_upper,
        };

        let limit = limit.unwrap_or(usize::MAX);

        let entries: Box<dyn Iterator<Item = anyhow::Result<(Slice, Slice)>> + '_> =
            match order.unwrap_or_default() {
                TsOrderBy::TimestampAsc => Box::new(self.range_latest_kv(lower, upper)),
                TsOrderBy::TimestampDesc => Box::new(self.range_latest_kv_rev(lower, upper)),
            };

        let mut results = vec![];
        for entry in entries.take(limit) {
            let (key, value) = entry?;

            let measurement =
                Measurement::decode_from_bytes(&key).context("unable to decode measurement key")?;
            let body: MeasurementBody =
                postcard::from_bytes(&value).context("unable to deserialise measurement body")?;

            results.push((body.tags, body.fields, measurement.timestamp));
        }

        Ok(results)
    }

    pub fn store_blob<'a>(
        &mut self,
        scope: Scope<'a>,
        blob: BlobRef<'_>,
    ) -> anyhow::Result<BlobId<'a>> {
        let ts = self.write_scope(scope.namespace, scope.database, scope.schema);

        // We should be able to easily change the hash used...
        let hash = Hash::Sha2(sha2::Sha256::digest(blob).into());

        let id = Key::blob_id()
            .namespace(scope.namespace)
            .database(scope.database)
            .schema(scope.schema)
            .hash(hash);

        self.put_at(&id, blob, ts)?;

        Ok(id)
    }

    pub fn link_blob(&mut self, path: Path<'_>, id: BlobId<'_>) -> anyhow::Result<()> {
        let ts = self.write_scope(path.namespace, path.database, path.schema);

        debug_assert_eq!(path.namespace, id.namespace, "scope should match");
        debug_assert_eq!(path.database, id.database, "scope should match");
        debug_assert_eq!(path.schema, id.schema, "scope should match");

        {
            let meta = Key::blob_meta()
                .namespace(path.namespace)
                .database(path.database)
                .schema(path.schema)
                .hash(id.hash)
                .path(path.path);

            self.put_at(&meta, &[], ts)?;
        }

        let id = id.encode().context("unable to encode id")?;

        self.put_at(&path, &id, ts)?;

        Ok(())
    }

    pub fn unlink_blob<'a>(
        &mut self,
        path: Path<'a>,
    ) -> anyhow::Result<Option<(BlobId<'a>, Option<Meta>)>> {
        let ts = self.write_scope(path.namespace, path.database, path.schema);

        let original_path = path;

        // Check if it actually exists...
        let Some((_k, id)) = self.get(&path)? else {
            return Ok(None);
        };

        let id: BlobId<'_> =
            StoreKey::decode_from_bytes(&id).context("unable to decode blob id")?;

        debug_assert_eq!(original_path.namespace, id.namespace, "scope should match");
        debug_assert_eq!(original_path.database, id.database, "scope should match");
        debug_assert_eq!(original_path.schema, id.schema, "scope should match");
        // Rebuild the hash so we have the correct lifetime. (We want to reuse the one from the path)
        let id = Key::blob_id()
            .namespace(original_path.namespace)
            .database(original_path.database)
            .schema(original_path.schema)
            .hash(id.hash);

        let metadata = {
            let meta = Key::blob_meta()
                .namespace(id.namespace)
                .database(id.database)
                .schema(id.schema)
                .hash(id.hash)
                .path(original_path.path);

            let metadata = self.get(&meta)?.map(|(_k, v)| v.to_vec());
            if metadata.is_some() {
                self.erase_at(&meta, ts)?;
            }

            metadata
        };

        self.erase_at(&path, ts)?;

        Ok(Some((id, metadata)))
    }

    /// Initially called relinking, this is how you unlink an existing path, and assign a new one.
    /// Note: You can assign _multiple_ paths to the same blob.
    pub fn move_blob(&mut self, old_path: Path<'_>, new_path: Path<'_>) -> anyhow::Result<()> {
        let (id, _meta) = self
            .unlink_blob(old_path)?
            .context("no blob at source path")?;

        self.link_blob(new_path, id)?;

        Ok(())
    }

    pub fn resolve_blob(&mut self, id: BlobId<'_>) -> anyhow::Result<Option<Blob>> {
        self.get(&id).map(|s| s.map(|(_k, v)| v.to_vec()))
    }

    pub fn resolve_path<'a>(
        &mut self,
        path: Path<'a>,
    ) -> anyhow::Result<Option<(Blob, BlobId<'a>)>> {
        let Some((_k, raw_id)) = self.get(&path)? else {
            return Ok(None);
        };
        let id: BlobId<'_> =
            StoreKey::decode_from_bytes(&raw_id).context("unable to decode blob id")?;

        let Some(blob) = self.resolve_blob(id)? else {
            return Ok(None);
        };

        // Reconstruct the id to detangle the lifetime
        let id = Key::blob_id()
            .namespace(path.namespace)
            .database(path.database)
            .schema(path.schema)
            .hash(id.hash);

        Ok(Some((blob, id)))
    }

    pub fn resolve_path_metadata(&mut self, path: Path<'_>) -> anyhow::Result<Option<Meta>> {
        let original_path = path;

        let Some((_k, raw_id)) = self.get(&path)? else {
            return Ok(None);
        };
        let id: BlobId<'_> =
            StoreKey::decode_from_bytes(&raw_id).context("unable to decode blob id")?;

        debug_assert_eq!(original_path.namespace, id.namespace, "scope should match");
        debug_assert_eq!(original_path.database, id.database, "scope should match");
        debug_assert_eq!(original_path.schema, id.schema, "scope should match");

        let meta = Key::blob_meta()
            .namespace(original_path.namespace)
            .database(original_path.database)
            .schema(original_path.schema)
            .hash(id.hash)
            .path(original_path.path);

        Ok(self.get(&meta)?.map(|(_k, v)| v.to_vec()))
    }

    pub fn list_paths(
        &mut self,
        scope: Scope<'_>,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<String>> {
        let prefix = Key::path()
            .namespace(scope.namespace)
            .database(scope.database)
            .schema(scope.schema)
            .encode()
            .context("unable to encode path prefix")?;

        let limit = limit.unwrap_or(usize::MAX);

        let mut paths = vec![];
        for key in self.prefix_latest(&prefix) {
            let key = key?;

            if paths.len() >= limit {
                break;
            }

            let path = Path::decode_from_bytes(&key).context("unable to decode path key")?;
            paths.push(String::from(path.path));
        }

        Ok(paths)
    }

    pub fn sem_update(&mut self, scope: Scope<'_>, update: Update) -> anyhow::Result<()> {
        // We're just calling this to track the scope, the functions inside the sem-engine call functions
        // that use the timestamp already embedded in the tx.
        let _ts = self.write_scope(scope.namespace, scope.database, scope.schema);

        let spargebra::Update {
            base_iri,
            operations,
        } = update.raw;

        for op in operations {
            use spargebra::GraphUpdateOperation as Op;

            match op {
                Op::InsertData { data } => {
                    semantic::sem_insert_data(&mut *self, scope, data)
                        .context("unable to insert semantic data")?;
                }
                Op::DeleteData { data } => {
                    semantic::sem_delete_data(&mut *self, scope, data)
                        .context("unable to delete semantic data")?;
                }
                Op::DeleteInsert {
                    delete,
                    insert,
                    using,
                    pattern,
                } => {
                    let solutions = {
                        let query = Query {
                            dataset: using,
                            base_iri: base_iri.clone(),
                            pattern: *pattern,
                            kind: crate::semantic::QueryKind::Select,
                        };

                        let solutions = self.sem_solution(scope, query, 0, usize::MAX)?;

                        Vec::from(solutions)
                    };

                    semantic::sem_delete_insert(&mut *self, scope, &delete, &insert, &solutions)
                        .context("unable to apply semantic delete/insert")?;
                }
                Op::Create { silent: _, graph } => {
                    semantic::sem_insert_graph_name(&mut *self, scope, &graph)
                        .context("unable to insert semantic graph name")?;
                }
                Op::Load {
                    silent: _,
                    source: _,
                    destination: _,
                } => {
                    #[expect(clippy::todo, reason = "TODO: implement SPARQL LOAD support")]
                    {
                        todo!("add support for loading")
                    }
                }
                Op::Drop {
                    silent: _,
                    graph: _,
                } => {
                    #[expect(clippy::todo, reason = "TODO: implement SPARQL DROP support")]
                    {
                        todo!("add support for dropping")
                    }
                }
                Op::Clear {
                    silent: _,
                    graph: _,
                } => {
                    #[expect(clippy::todo, reason = "TODO: implement SPARQL CLEAR support")]
                    {
                        todo!("add support for clear")
                    }
                }
            }
        }

        Ok(())
    }

    pub fn sem_solution(
        &mut self,
        scope: Scope<'_>,
        query: Query,
        skip: usize,
        limit: usize,
    ) -> anyhow::Result<crate::semantic::QuerySolution> {
        let QueryResults::Solutions(it) =
            semantic::sem_eval(&mut *self, scope, query).context("unable to evaluate query")?
        else {
            anyhow::bail!("[Internal Error] Query was checked but still returned the wrong thing");
        };

        let variables = it
            .variables()
            .iter()
            .map(|var| var.clone().into_string())
            .collect::<Vec<_>>();

        let solutions = it.skip(skip).take(limit);

        let mut response = vec![];
        let mut errors = vec![];

        for solution in solutions {
            let solution = match solution {
                Ok(s) => s,
                Err(err) => {
                    errors.push(err);
                    continue;
                }
            };

            // Stop processing solutions if we've already got errors.
            if !errors.is_empty() {
                continue;
            }

            let solution = solution.values().to_vec();

            response.push(solution);
        }

        Ok(crate::semantic::QuerySolution {
            variables,
            solutions: response,
        })
    }

    pub fn sem_graph(
        &mut self,
        scope: Scope<'_>,
        query: Query,
        skip: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        let QueryResults::Graph(it) =
            semantic::sem_eval(&mut *self, scope, query).context("unable to evaluate query")?
        else {
            anyhow::bail!("[Internal Error] Query was checked but still returned the wrong thing");
        };

        let triples = it.skip(skip).take(limit);

        let mut response = vec![];
        let mut errors = vec![];

        for triple in triples {
            let triple = match triple {
                Ok(t) => t,
                Err(err) => {
                    errors.push(err);
                    continue;
                }
            };

            // Stop processing triples if we've already got errors.
            if !errors.is_empty() {
                continue;
            }

            response.push((
                triple.subject.to_string(),
                triple.predicate.to_string(),
                triple.object.to_string(),
            ));
        }

        Ok(response)
    }

    pub fn sem_ask(&mut self, scope: Scope<'_>, query: Query) -> anyhow::Result<bool> {
        let QueryResults::Boolean(answer) =
            semantic::sem_eval(&mut *self, scope, query).context("unable to evaluate query")?
        else {
            anyhow::bail!("[Internal Error] Query was checked but still returned the wrong thing");
        };

        Ok(answer)
    }
}
