//! Typed access to a `tb` table, plus lazy iterators over its rows and keys.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use core::borrow::Borrow;
use core::marker::PhantomData;

use crate::{Codec, Postcard, Result, Sri};

use crate::db::{Cursor, Scope, TbOrderBy, tb_append, tb_count, tb_delete, tb_get, tb_list};

/// Encode a value into the raw entity-id bytes used to key a table.
pub trait AsEid {
    /// The raw entity-id bytes for this value.
    fn as_eid(&self) -> Vec<u8>;
}

/// A key type that can be reconstructed from the raw entity-id bytes of a
/// stored row, as well as encoded into them.
pub trait TableKey: AsEid + Sized {
    /// Reconstruct a key from its raw entity-id bytes, as stored in the table.
    fn from_eid(eid: &[u8]) -> Self;
}

impl AsEid for str {
    fn as_eid(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl AsEid for String {
    fn as_eid(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl TableKey for String {
    fn from_eid(eid: &[u8]) -> Self {
        String::from_utf8_lossy(eid).into_owned()
    }
}

impl AsEid for [u8] {
    fn as_eid(&self) -> Vec<u8> {
        self.to_vec()
    }
}

impl AsEid for Vec<u8> {
    fn as_eid(&self) -> Vec<u8> {
        self.clone()
    }
}

impl TableKey for Vec<u8> {
    fn from_eid(eid: &[u8]) -> Self {
        eid.to_vec()
    }
}

impl AsEid for Sri {
    fn as_eid(&self) -> Vec<u8> {
        self.to_bytes().to_vec()
    }
}

impl TableKey for Sri {
    fn from_eid(eid: &[u8]) -> Self {
        let mut bytes = [0u8; 16];
        let n = eid.len().min(16);
        bytes[..n].copy_from_slice(&eid[..n]);
        Sri::from_bytes(bytes)
    }
}

/// A typed handle to a `tb` table, storing serde values `V` keyed by `K`.
///
/// Construct one with [`Table::new`] (or [`Table::new_in`] for a non-default
/// [`Scope`]) and operate on it via [`get`](Self::get),
/// [`insert`](Self::insert), [`iter`](Self::iter), etc.
pub struct Table<V, K = String, C = Postcard> {
    scope: Scope,
    name: &'static str,
    _marker: PhantomData<(V, K, C)>,
}

impl<V, K, C> Table<V, K, C> {
    /// A handle to the table `name` in the default private [`Scope`].
    pub const fn new(name: &'static str) -> Self {
        Self::new_in(name, Scope::private())
    }

    /// A handle to the table `name` in an explicit `scope`.
    pub const fn new_in(name: &'static str, scope: Scope) -> Self {
        Self {
            scope,
            name,
            _marker: PhantomData,
        }
    }

    fn scope(&self) -> Scope {
        self.scope.clone()
    }
}

impl<V, K, C> Table<V, K, C>
where
    V: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    /// Fetches the value stored under `key`, or `None` if there is no such row.
    pub fn get<Q>(&self, key: &Q) -> Result<Option<V>>
    where
        K: Borrow<Q>,
        Q: AsEid + ?Sized,
    {
        let mut req = [0u8; 256];
        let mut resp = alloc::vec![0u8; 4096];
        tb_get(
            self.scope(),
            String::from(self.name),
            key.as_eid(),
            &mut req,
            &mut resp,
        )?
        .map(|b| C::decode(&b))
        .transpose()
    }

    /// Inserts `val` under a host-generated entity id.
    pub fn insert(&self, val: &V) -> Result<()> {
        self.insert_impl(None, C::encode(val)?)
    }

    /// Inserts `val` under `key`, replacing any existing value.
    pub fn insert_with<Q>(&self, key: &Q, val: &V) -> Result<()>
    where
        K: Borrow<Q>,
        Q: AsEid + ?Sized,
    {
        self.insert_impl(Some(key.as_eid()), C::encode(val)?)
    }

    /// Neither entry point reports the row id back, so both append: the host
    /// batches the write into the handler's transaction instead of spending a
    /// round trip to tell us an id we would drop.
    fn insert_impl(&self, key: Option<Vec<u8>>, val: Vec<u8>) -> Result<()> {
        let mut req = alloc::vec![0u8; val.len() + 1024];
        tb_append(self.scope(), String::from(self.name), key, val, &mut req)
            .map_err(|_| "Table::insert")?;
        Ok(())
    }

    /// Deletes the row keyed `key`, if any.
    pub fn delete<Q>(&self, key: &Q) -> Result<()>
    where
        K: Borrow<Q>,
        Q: AsEid + ?Sized,
    {
        let mut buf = [0u8; 256];
        tb_delete(
            self.scope(),
            String::from(self.name),
            key.as_eid(),
            &mut buf,
        )
        .map_err(|_| "Table::delete")?;
        Ok(())
    }

    /// The number of rows in the table.
    pub fn count(&self) -> Result<usize> {
        let mut req = [0u8; 256];
        let mut resp = [0u8; 64];
        tb_count(self.scope(), String::from(self.name), &mut req, &mut resp)
            .map_err(|_| "Table::count")
    }

    /// All values, collected in ascending key order.
    pub fn list(&self) -> Result<Vec<V>> {
        let mut out = Vec::new();
        self.for_each(|v| out.push(v))?;
        Ok(out)
    }

    /// Like [`Self::list`], but returns values in descending key order.
    pub fn list_rev(&self) -> Result<Vec<V>> {
        let mut out = Vec::new();
        self.for_each_rev(|v| out.push(v))?;
        Ok(out)
    }

    /// Visit every value, in ascending key order.
    pub fn for_each<F: FnMut(V)>(&self, mut f: F) -> Result<()> {
        let scope = self.scope();
        for_each_ordered::<_, V, C>(&scope, self.name, TbOrderBy::KeyAsc, |_eid, val: V| f(val))
    }

    /// Visit every value, in descending key order.
    pub fn for_each_rev<F: FnMut(V)>(&self, mut f: F) -> Result<()> {
        let scope = self.scope();
        for_each_ordered::<_, V, C>(&scope, self.name, TbOrderBy::KeyDesc, |_eid, val: V| f(val))
    }

    /// Internal iterating through undecoded `(eid, value)` byte rows.
    fn iter_raw(&self, order: TbOrderBy) -> IterRaw {
        IterRaw::new(self.scope(), String::from(self.name), order)
    }

    /// Lazily stream `(key, value)` pairs in ascending key order.
    pub fn iter(&self) -> Iter<V, K, C> {
        Iter {
            raw: self.iter_raw(TbOrderBy::KeyAsc),
            _marker: PhantomData,
        }
    }

    /// Like [`Self::iter`], but streams `(key, value)` pairs in descending key order.
    pub fn iter_rev(&self) -> Iter<V, K, C> {
        Iter {
            raw: self.iter_raw(TbOrderBy::KeyDesc),
            _marker: PhantomData,
        }
    }

    /// Lazily stream the table's keys in ascending order.
    pub fn keys(&self) -> Keys<K> {
        Keys {
            raw: self.iter_raw(TbOrderBy::KeyAsc),
            _marker: PhantomData,
        }
    }

    /// Like [`Self::keys`], but streams the table's keys in descending order.
    pub fn keys_rev(&self) -> Keys<K> {
        Keys {
            raw: self.iter_raw(TbOrderBy::KeyDesc),
            _marker: PhantomData,
        }
    }

    /// All keys in the table, collected.
    pub fn ids(&self) -> Result<Vec<K>>
    where
        K: TableKey,
    {
        self.keys().collect()
    }

    /// Materialise the whole table into a `key -> value` map by collecting
    /// [`Self::iter`].
    /// Prefer the lazy [`Self::iter`]/[`Self::for_each`] for large tables.
    pub fn to_map(&self) -> Result<BTreeMap<K, V>>
    where
        K: TableKey + Ord,
    {
        self.iter().collect()
    }
}

/// Internal iterator that doesn't decode anything,
/// _just_ deals with getting rows lazily out of the table.
struct IterRaw {
    scope: Scope,
    table: String,
    cursor: Option<Cursor>,
    order: TbOrderBy,
    done: bool,
}

impl IterRaw {
    fn new(scope: Scope, table: String, order: TbOrderBy) -> Self {
        Self {
            scope,
            table,
            cursor: None,
            order,
            done: false,
        }
    }
}

impl Iterator for IterRaw {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut req = [0u8; 256];
        let mut resp = alloc::vec![0u8; 4096];
        let row = tb_list(
            self.scope.clone(),
            self.table.clone(),
            self.cursor.take(),
            Some(1),
            Some(self.order),
            &mut req,
            &mut resp,
        );

        let rows = match row {
            Ok(rows) => rows,
            Err(_) => {
                self.done = true;
                return Some(Err("tb_list"));
            }
        };
        let Some((eid, val)) = rows.into_iter().next() else {
            self.done = true;
            return None;
        };
        self.cursor = Some(Cursor::After(eid.clone()));
        Some(Ok((eid, val)))
    }
}

/// Lazy iterator over a [`Table`] yielding decoded `(key, value)` pairs
pub struct Iter<V, K, C = Postcard> {
    raw: IterRaw,
    _marker: PhantomData<(V, K, C)>,
}

impl<V, K, C> Iterator for Iter<V, K, C>
where
    V: serde::de::DeserializeOwned,
    K: TableKey,
    C: Codec,
{
    type Item = Result<(K, V)>;

    fn next(&mut self) -> Option<Self::Item> {
        let (eid, val) = match self.raw.next()? {
            Ok(pair) => pair,
            Err(e) => return Some(Err(e)),
        };
        Some(C::decode::<V>(&val).map(|v| (K::from_eid(&eid), v)))
    }
}

/// Lazy iterator over a [`Table`]'s keys
pub struct Keys<K = String> {
    raw: IterRaw,
    _marker: PhantomData<K>,
}

impl<K> Iterator for Keys<K>
where
    K: TableKey,
{
    type Item = Result<K>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.raw.next()? {
            Ok((eid, _val)) => Some(Ok(K::from_eid(&eid))),
            Err(e) => Some(Err(e)),
        }
    }
}

/// Visits every row of `table` in `scope` in ascending key order, passing the
/// raw entity id and the [`Postcard`]-decoded value; rows that fail to decode
/// are skipped.
pub fn for_each<F, T>(scope: &Scope, table: &str, f: F) -> Result<()>
where
    F: for<'a> FnMut(&'a [u8], T),
    T: serde::de::DeserializeOwned,
{
    for_each_ordered::<_, T, Postcard>(scope, table, TbOrderBy::KeyAsc, f)
}

fn for_each_ordered<F, T, C>(scope: &Scope, table: &str, order: TbOrderBy, mut f: F) -> Result<()>
where
    F: for<'a> FnMut(&'a [u8], T),
    T: serde::de::DeserializeOwned,
    C: Codec,
{
    for_each_raw_ordered(scope, table, order, |eid, val| {
        if let Ok(s) = C::decode::<T>(&val) {
            f(eid, s);
        }
    })
}

/// Visits every `(eid, value)` row of `table` in `scope` in ascending key
/// order, without decoding the values.
pub fn for_each_raw<F>(scope: &Scope, table: &str, f: F) -> Result<()>
where
    F: for<'a> FnMut(&'a [u8], Vec<u8>),
{
    for_each_raw_ordered(scope, table, TbOrderBy::KeyAsc, f)
}

fn for_each_raw_ordered<F>(scope: &Scope, table: &str, order: TbOrderBy, mut f: F) -> Result<()>
where
    F: for<'a> FnMut(&'a [u8], Vec<u8>),
{
    for row in IterRaw::new(scope.clone(), String::from(table), order) {
        let (eid, val) = row?;
        f(eid.as_slice(), val);
    }
    Ok(())
}
