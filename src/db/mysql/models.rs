use std::{self, cell::RefCell, collections::HashMap, fmt, ops::Deref, sync::Arc};

use diesel::{
    connection::TransactionManager,
    delete,
    dsl::max,
    expression::sql_literal::sql,
    insert_into,
    mysql::MysqlConnection,
    r2d2::{ConnectionManager, PooledConnection},
    sql_query,
    sql_types::{BigInt, Integer, Nullable, Text},
    update, Connection, ExpressionMethods, GroupByDsl, OptionalExtension, QueryDsl, RunQueryDsl,
};
#[cfg(test)]
use diesel_logger::LoggingConnection;
use futures::{future, lazy};

use super::{
    batch,
    diesel_ext::LockInShareModeDsl,
    pool::CollectionCache,
    schema::{bso, collections, user_collections},
};
use db::{
    error::{DbError, DbErrorKind},
    params, results,
    util::SyncTimestamp,
    Db, DbFuture, Sorting,
};
use web::extractors::{BsoQueryParams, HawkIdentifier};

no_arg_sql_function!(last_insert_id, Integer);

pub type Result<T> = std::result::Result<T, DbError>;
type Conn = PooledConnection<ConnectionManager<MysqlConnection>>;

/// The ttl to use for rows that are never supposed to expire (in seconds)
pub const DEFAULT_BSO_TTL: u32 = 2_100_000_000;

#[derive(Debug)]
pub enum CollectionLock {
    Read,
    Write,
}

/// Per session Db metadata
#[derive(Debug, Default)]
struct MysqlDbSession {
    /// The "current time" on the server used for this session's operations
    timestamp: SyncTimestamp,
    /// Cache of collection modified timestamps per (user_id, collection_id)
    coll_modified_cache: HashMap<(u32, i32), SyncTimestamp>,
    /// Currently locked collections
    coll_locks: HashMap<(u32, i32), CollectionLock>,
}

#[derive(Clone, Debug)]
pub struct MysqlDb {
    /// Synchronous Diesel calls are executed in a tokio ThreadPool to satisfy
    /// the Db trait's asynchronous interface.
    ///
    /// Arc<MysqlDbInner> provides a Clone impl utilized for safely moving to
    /// the thread pool but does not provide Send as the underlying db
    /// conn. structs are !Sync (Arc requires both for Send). See the Send impl
    /// below.
    pub(super) inner: Arc<MysqlDbInner>,

    /// Pool level cache of collection_ids and their names
    coll_cache: Arc<CollectionCache>,
}

/// Despite the db conn structs being !Sync (see Arc<MysqlDbInner> above) we
/// don't spawn multiple MysqlDb calls at a time in the thread pool. Calls are
/// queued to the thread pool via Futures, naturally serialized.
unsafe impl Send for MysqlDb {}

pub struct MysqlDbInner {
    #[cfg(not(test))]
    pub(super) conn: Conn,
    #[cfg(test)]
    pub(super) conn: LoggingConnection<Conn>,

    session: RefCell<MysqlDbSession>,

    thread_pool: Arc<::tokio_threadpool::ThreadPool>,
}

impl fmt::Debug for MysqlDbInner {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MysqlDbInner {{ session: {:?} }}", self.session)
    }
}

impl Deref for MysqlDb {
    type Target = MysqlDbInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl MysqlDb {
    pub fn new(
        conn: Conn,
        thread_pool: Arc<::tokio_threadpool::ThreadPool>,
        coll_cache: Arc<CollectionCache>,
    ) -> Self {
        let inner = MysqlDbInner {
            #[cfg(not(test))]
            conn,
            #[cfg(test)]
            conn: LoggingConnection::new(conn),
            session: RefCell::new(Default::default()),
            thread_pool,
        };
        MysqlDb {
            inner: Arc::new(inner),
            coll_cache,
        }
    }

    /// APIs for collection-level locking
    ///
    /// Explicitly lock the matching row in the user_collections table. Read
    /// locks do SELECT ... LOCK IN SHARE MODE and write locks do SELECT
    /// ... FOR UPDATE.
    ///
    /// In theory it would be possible to use serializable transactions rather
    /// than explicit locking, but our ops team have expressed concerns about
    /// the efficiency of that approach at scale.
    pub fn lock_for_read_sync(&self, params: params::LockCollection) -> Result<()> {
        let user_id = params.user_id.legacy_id as u32;
        let collection_id =
            self.get_collection_id(&params.collection)
                .or_else(|e| match e.kind() {
                    // If the collection doesn't exist, we still want to start a
                    // transaction so it will continue to not exist.
                    DbErrorKind::CollectionNotFound => Ok(0),
                    _ => Err(e),
                })?;
        // If we already have a read or write lock then it's safe to
        // use it as-is.
        if self
            .session
            .borrow()
            .coll_locks
            .get(&(user_id, collection_id))
            .is_some()
        {
            return Ok(());
        }

        // Lock the db
        self.begin()?;
        let modified = user_collections::table
            .select(user_collections::modified)
            .filter(user_collections::user_id.eq(user_id as i32))
            .filter(user_collections::collection_id.eq(collection_id))
            .lock_in_share_mode()
            .first(&self.conn)
            .optional()?;
        if let Some(modified) = modified {
            let modified = SyncTimestamp::from_i64(modified)?;
            self.session
                .borrow_mut()
                .coll_modified_cache
                .insert((user_id, collection_id), modified);
        }
        // XXX: who's responsible for unlocking (removing the entry)
        self.session
            .borrow_mut()
            .coll_locks
            .insert((user_id, collection_id), CollectionLock::Read);
        Ok(())
    }

    pub fn lock_for_write_sync(&self, params: params::LockCollection) -> Result<()> {
        let user_id = params.user_id.legacy_id as u32;
        let collection_id = self.get_or_create_collection_id(&params.collection)?;
        if let Some(CollectionLock::Read) = self
            .session
            .borrow()
            .coll_locks
            .get(&(user_id, collection_id))
        {
            Err(DbError::internal("Can't escalate read-lock to write-lock"))?
        }

        // Lock the db
        self.begin()?;
        let modified = user_collections::table
            .select(user_collections::modified)
            .filter(user_collections::user_id.eq(user_id as i32))
            .filter(user_collections::collection_id.eq(collection_id))
            .for_update()
            .first(&self.conn)
            .optional()?;
        if let Some(modified) = modified {
            let modified = SyncTimestamp::from_i64(modified)?;
            // Forbid the write if it would not properly incr the timestamp
            if modified >= self.timestamp() {
                Err(DbErrorKind::Conflict)?
            }
            self.session
                .borrow_mut()
                .coll_modified_cache
                .insert((user_id, collection_id), modified);
        }
        self.session
            .borrow_mut()
            .coll_locks
            .insert((user_id, collection_id), CollectionLock::Write);
        Ok(())
    }

    pub(super) fn begin(&self) -> Result<()> {
        Ok(self
            .conn
            .transaction_manager()
            .begin_transaction(&self.conn)?)
    }

    pub fn commit_sync(&self) -> Result<()> {
        Ok(self
            .conn
            .transaction_manager()
            .commit_transaction(&self.conn)?)
    }

    pub fn rollback_sync(&self) -> Result<()> {
        Ok(self
            .conn
            .transaction_manager()
            .rollback_transaction(&self.conn)?)
    }

    pub fn delete_storage_sync(&self, user_id: HawkIdentifier) -> Result<()> {
        let user_id = user_id.legacy_id;
        delete(bso::table)
            .filter(bso::user_id.eq(user_id as i32))
            .execute(&self.conn)?;
        delete(user_collections::table)
            .filter(user_collections::user_id.eq(user_id as i32))
            .execute(&self.conn)?;
        Ok(())
    }

    pub fn delete_collection_sync(
        &self,
        params: params::DeleteCollection,
    ) -> Result<SyncTimestamp> {
        let user_id = params.user_id.legacy_id;
        let collection_id = self.get_collection_id(&params.collection)?;
        let mut count = delete(bso::table)
            .filter(bso::user_id.eq(user_id as i32))
            .filter(bso::collection_id.eq(&collection_id))
            .execute(&self.conn)?;
        count += delete(user_collections::table)
            .filter(user_collections::user_id.eq(user_id as i32))
            .filter(user_collections::collection_id.eq(&collection_id))
            .execute(&self.conn)?;
        if count == 0 {
            Err(DbErrorKind::CollectionNotFound)?
        }
        self.get_storage_timestamp_sync(params.user_id)
    }

    pub(super) fn create_collection(&self, name: &str) -> Result<i32> {
        // XXX: handle concurrent attempts at inserts
        let id = self.conn.transaction(|| {
            sql_query("INSERT INTO collections (name) VALUES (?)")
                .bind::<Text, _>(name)
                .execute(&self.conn)?;
            collections::table.select(last_insert_id).first(&self.conn)
        })?;
        self.coll_cache.put(id, name.to_owned())?;
        Ok(id)
    }

    fn get_or_create_collection_id(&self, name: &str) -> Result<i32> {
        self.get_collection_id(name).or_else(|e| match e.kind() {
            DbErrorKind::CollectionNotFound => self.create_collection(name),
            _ => Err(e),
        })
    }

    pub(super) fn get_collection_id(&self, name: &str) -> Result<i32> {
        if let Some(id) = self.coll_cache.get_id(name)? {
            return Ok(id);
        }

        let id = sql_query("SELECT id FROM collections WHERE name = ?")
            .bind::<Text, _>(name)
            .get_result::<IdResult>(&self.conn)
            .optional()?
            .ok_or(DbErrorKind::CollectionNotFound)?
            .id;
        self.coll_cache.put(id, name.to_owned())?;
        Ok(id)
    }

    fn _get_collection_name(&self, id: i32) -> Result<String> {
        let name = if let Some(name) = self.coll_cache.get_name(id)? {
            name
        } else {
            sql_query("SELECT name FROM collections where id = ?")
                .bind::<Integer, _>(&id)
                .get_result::<NameResult>(&self.conn)
                .optional()?
                .ok_or(DbErrorKind::CollectionNotFound)?
                .name
        };
        Ok(name)
    }

    pub fn put_bso_sync(&self, bso: params::PutBso) -> Result<results::PutBso> {
        /*
        if bso.payload.is_none() && bso.sortindex.is_none() && bso.ttl.is_none() {
            // XXX: go returns an error here (ErrNothingToDo), and is treated
            // as other errors
            return Ok(());
        }
        */

        let collection_id = self.get_or_create_collection_id(&bso.collection)?;
        let user_id: u64 = bso.user_id.legacy_id;
        let timestamp = self.timestamp().as_i64();

        // XXX: consider mysql ON DUPLICATE KEY UPDATE?
        self.conn.transaction(|| {
            let q = r#"
                SELECT 1 as count FROM bso
                WHERE user_id = ? AND collection_id = ? AND id = ?
            "#;
            let exists = sql_query(q)
                .bind::<Integer, _>(user_id as i32) // XXX:
                .bind::<Integer, _>(&collection_id)
                .bind::<Text, _>(&bso.id)
                .get_result::<Count>(&self.conn)
                .optional()?
                .is_some();

            if exists {
                update(bso::table)
                    .filter(bso::user_id.eq(user_id as i32)) // XXX:
                    .filter(bso::collection_id.eq(&collection_id))
                    .filter(bso::id.eq(&bso.id))
                    .set(put_bso_as_changeset(&bso, timestamp))
                    .execute(&self.conn)?;
            } else {
                let payload = bso.payload.as_ref().map(Deref::deref).unwrap_or_default();
                let sortindex = bso.sortindex;
                let ttl = bso.ttl.map_or(DEFAULT_BSO_TTL, |ttl| ttl);
                insert_into(bso::table)
                    .values((
                        bso::user_id.eq(user_id as i32), // XXX:
                        bso::collection_id.eq(&collection_id),
                        bso::id.eq(&bso.id),
                        bso::sortindex.eq(sortindex),
                        bso::payload.eq(payload),
                        bso::modified.eq(timestamp),
                        bso::expiry.eq(timestamp + (ttl as i64 * 1000)),
                    ))
                    .execute(&self.conn)?;
            }
            self.touch_collection(user_id as u32, collection_id)
        })
    }

    pub fn get_bsos_sync(&self, params: params::GetBsos) -> Result<results::GetBsos> {
        let user_id = params.user_id.legacy_id as i32;
        let collection_id = self.get_collection_id(&params.collection)?;
        let BsoQueryParams {
            newer,
            older,
            sort,
            limit,
            offset,
            ids,
            ..
        } = params.params;

        let mut query = bso::table
            .select((
                bso::id,
                bso::modified,
                bso::payload,
                bso::sortindex,
                bso::expiry,
            ))
            .filter(bso::user_id.eq(user_id))
            .filter(bso::collection_id.eq(collection_id as i32)) // XXX:
            .filter(bso::expiry.gt(self.timestamp().as_i64()))
            .into_boxed();

        if let Some(older) = older {
            query = query.filter(bso::modified.lt(older.as_i64()));
        }
        if let Some(newer) = newer {
            query = query.filter(bso::modified.gt(newer.as_i64()));
        }

        if !ids.is_empty() {
            query = query.filter(bso::id.eq_any(ids));
        }

        query = match sort {
            Sorting::Index => query.order(bso::sortindex.desc()),
            Sorting::Newest => query.order(bso::modified.desc()),
            Sorting::Oldest => query.order(bso::modified.asc()),
            _ => query,
        };

        let limit = limit.map(|limit| i64::from(limit)).unwrap_or(-1);
        // fetch an extra row to detect if there are more rows that
        // match the query conditions
        query = query.limit(if limit >= 0 { limit + 1 } else { limit });

        let offset = offset.unwrap_or(0) as i64;
        if offset != 0 {
            // XXX: copy over this optimization:
            // https://github.com/mozilla-services/server-syncstorage/blob/a0f8117/syncstorage/storage/sql/__init__.py#L404
            query = query.offset(offset);
        }
        let mut bsos = query.load::<results::GetBso>(&self.conn)?;

        // XXX: an additional get_collection_timestamp is done here in
        // python to trigger potential CollectionNotFoundErrors
        //if bsos.len() == 0 {
        //}

        let next_offset = if limit >= 0 && bsos.len() > limit as usize {
            bsos.pop();
            Some(limit + offset)
        } else {
            None
        };

        Ok(results::GetBsos {
            items: bsos,
            offset: next_offset,
        })
    }

    pub fn get_bso_ids_sync(&self, params: params::GetBsos) -> Result<results::GetBsoIds> {
        // XXX: should be a more efficient select of only the id column
        let result = self.get_bsos_sync(params)?;
        Ok(results::GetBsoIds {
            items: result.items.into_iter().map(|bso| bso.id).collect(),
            offset: result.offset,
        })
    }

    pub fn get_bso_sync(&self, params: params::GetBso) -> Result<Option<results::GetBso>> {
        let user_id = params.user_id.legacy_id;
        let collection_id = self.get_collection_id(&params.collection)?;
        Ok(bso::table
            .select((
                bso::id,
                bso::modified,
                bso::payload,
                bso::sortindex,
                bso::expiry,
            ))
            .filter(bso::user_id.eq(user_id as i32))
            .filter(bso::collection_id.eq(&collection_id))
            .filter(bso::id.eq(&params.id))
            .filter(bso::expiry.ge(self.timestamp().as_i64()))
            .get_result::<results::GetBso>(&self.conn)
            .optional()?)
    }

    pub fn delete_bso_sync(&self, params: params::DeleteBso) -> Result<results::DeleteBso> {
        let user_id = params.user_id.legacy_id;
        let collection_id = self.get_collection_id(&params.collection)?;
        let affected_rows = delete(bso::table)
            .filter(bso::user_id.eq(user_id as i32))
            .filter(bso::collection_id.eq(&collection_id))
            .filter(bso::id.eq(params.id))
            .filter(bso::expiry.gt(&self.timestamp().as_i64()))
            .execute(&self.conn)?;
        if affected_rows == 0 {
            Err(DbErrorKind::BsoNotFound)?
        }
        self.touch_collection(user_id as u32, collection_id)
    }

    pub fn delete_bsos_sync(&self, params: params::DeleteBsos) -> Result<results::DeleteBsos> {
        let user_id = params.user_id.legacy_id;
        let collection_id = self.get_collection_id(&params.collection)?;
        delete(bso::table)
            .filter(bso::user_id.eq(user_id as i32))
            .filter(bso::collection_id.eq(&collection_id))
            .filter(bso::id.eq_any(params.ids))
            .execute(&self.conn)?;
        self.touch_collection(user_id as u32, collection_id)
    }

    pub fn post_bsos_sync(&self, input: params::PostBsos) -> Result<results::PostBsos> {
        let collection_id = self.get_or_create_collection_id(&input.collection)?;
        let mut result = results::PostBsos {
            modified: self.timestamp(),
            success: Default::default(),
            failed: input.failed,
        };

        for pbso in input.bsos {
            let id = pbso.id;
            let put_result = self.put_bso_sync(params::PutBso {
                user_id: input.user_id.clone(),
                collection: input.collection.clone(),
                id: id.clone(),
                payload: pbso.payload,
                sortindex: pbso.sortindex,
                ttl: pbso.ttl,
            });
            // XXX: python version doesn't report failures from db layer..
            // XXX: sanitize to.to_string()?
            match put_result {
                Ok(_) => result.success.push(id),
                Err(e) => {
                    result.failed.insert(id, e.to_string());
                }
            }
        }
        self.touch_collection(input.user_id.legacy_id as u32, collection_id)?;
        Ok(result)
    }

    pub fn get_storage_timestamp_sync(&self, user_id: HawkIdentifier) -> Result<SyncTimestamp> {
        let user_id = user_id.legacy_id as i32;
        let modified = user_collections::table
            .select(max(user_collections::modified))
            .filter(user_collections::user_id.eq(user_id))
            .first::<Option<i64>>(&self.conn)?
            .unwrap_or_default();
        Ok(SyncTimestamp::from_i64(modified)?)
    }

    pub fn get_collection_timestamp_sync(
        &self,
        params: params::GetCollectionTimestamp,
    ) -> Result<SyncTimestamp> {
        let user_id = params.user_id.legacy_id as u32;
        let collection_id = self.get_collection_id(&params.collection)?;
        if let Some(modified) = self
            .session
            .borrow()
            .coll_modified_cache
            .get(&(user_id, collection_id))
        {
            return Ok(*modified);
        }
        user_collections::table
            .select(user_collections::modified)
            .filter(user_collections::user_id.eq(user_id as i32))
            .filter(user_collections::collection_id.eq(collection_id))
            .first(&self.conn)
            .optional()?
            .ok_or_else(|| DbErrorKind::CollectionNotFound.into())
    }

    pub fn get_bso_timestamp_sync(&self, params: params::GetBsoTimestamp) -> Result<SyncTimestamp> {
        let user_id = params.user_id.legacy_id;
        let collection_id = self.get_collection_id(&params.collection)?;
        let modified = bso::table
            .select(bso::modified)
            .filter(bso::user_id.eq(user_id as i32))
            .filter(bso::collection_id.eq(&collection_id))
            .filter(bso::id.eq(&params.id))
            .first::<i64>(&self.conn)
            .optional()?
            .unwrap_or_default();
        Ok(SyncTimestamp::from_i64(modified)?)
    }

    pub fn get_collection_timestamps_sync(
        &self,
        user_id: HawkIdentifier,
    ) -> Result<results::GetCollectionTimestamps> {
        let modifieds =
            sql_query("SELECT collection_id, modified FROM user_collections WHERE user_id = ?")
                .bind::<Integer, _>(user_id.legacy_id as i32)
                .load::<UserCollectionsResult>(&self.conn)?
                .into_iter()
                .map(|cr| {
                    SyncTimestamp::from_i64(cr.modified).and_then(|ts| Ok((cr.collection_id, ts)))
                })
                .collect::<Result<HashMap<_, _>>>()?;
        self.map_collection_names(modifieds)
    }

    fn map_collection_names<T>(&self, by_id: HashMap<i32, T>) -> Result<HashMap<String, T>> {
        let mut names = self.load_collection_names(by_id.keys())?;
        by_id
            .into_iter()
            .map(|(id, value)| {
                names
                    .remove(&id)
                    .map(|name| (name, value))
                    .ok_or_else(|| DbError::internal("load_collection_names get"))
            })
            .collect()
    }

    fn load_collection_names<'a>(
        &self,
        collection_ids: impl Iterator<Item = &'a i32>,
    ) -> Result<HashMap<i32, String>> {
        let mut names = HashMap::new();
        let mut uncached = Vec::new();
        for &id in collection_ids {
            if let Some(name) = self.coll_cache.get_name(id)? {
                names.insert(id, name);
            } else {
                uncached.push(id);
            }
        }

        if !uncached.is_empty() {
            let result = collections::table
                .select((collections::id, collections::name))
                .filter(collections::id.eq_any(uncached))
                .load::<(i32, String)>(&self.conn)?;

            for (id, name) in result {
                names.insert(id, name.clone());
                self.coll_cache.put(id, name)?;
            }
        }

        Ok(names)
    }

    pub(super) fn touch_collection(
        &self,
        user_id: u32,
        collection_id: i32,
    ) -> Result<SyncTimestamp> {
        let upsert = r#"
                INSERT INTO user_collections (user_id, collection_id, modified)
                VALUES (?, ?, ?)
                ON DUPLICATE KEY UPDATE modified = ?
        "#;
        sql_query(upsert)
            .bind::<Integer, _>(user_id as i32)
            .bind::<Integer, _>(&collection_id)
            .bind::<BigInt, _>(&self.timestamp().as_i64())
            .bind::<BigInt, _>(&self.timestamp().as_i64())
            .execute(&self.conn)?;
        Ok(self.timestamp())
    }

    pub fn get_storage_usage_sync(
        &self,
        user_id: HawkIdentifier,
    ) -> Result<results::GetStorageUsage> {
        let total_size = bso::table
            .select(sql::<Nullable<BigInt>>("SUM(LENGTH(payload))"))
            .filter(bso::user_id.eq(user_id.legacy_id as i32))
            .filter(bso::expiry.gt(&self.timestamp().as_i64()))
            .get_result::<Option<i64>>(&self.conn)?;
        Ok(total_size.unwrap_or_default() as u64)
    }

    pub fn get_collection_usage_sync(
        &self,
        user_id: HawkIdentifier,
    ) -> Result<results::GetCollectionUsage> {
        let counts = bso::table
            .select((bso::collection_id, sql::<BigInt>("SUM(LENGTH(payload))")))
            .filter(bso::user_id.eq(user_id.legacy_id as i32))
            .filter(bso::expiry.gt(&self.timestamp().as_i64()))
            .group_by(bso::collection_id)
            .load(&self.conn)?
            .into_iter()
            .collect();
        self.map_collection_names(counts)
    }

    pub fn get_collection_counts_sync(
        &self,
        user_id: HawkIdentifier,
    ) -> Result<results::GetCollectionCounts> {
        let counts = bso::table
            .select((bso::collection_id, sql::<BigInt>("COUNT(collection_id)")))
            .filter(bso::user_id.eq(user_id.legacy_id as i32))
            .filter(bso::expiry.gt(&self.timestamp().as_i64()))
            .group_by(bso::collection_id)
            .load(&self.conn)?
            .into_iter()
            .collect();
        self.map_collection_names(counts)
    }

    batch_db_method!(create_batch_sync, create, CreateBatch);
    batch_db_method!(validate_batch_sync, validate, ValidateBatch);
    batch_db_method!(append_to_batch_sync, append, AppendToBatch);
    batch_db_method!(commit_batch_sync, commit, CommitBatch);

    pub fn get_batch_sync(&self, params: params::GetBatch) -> Result<Option<results::GetBatch>> {
        batch::get(&self, params)
    }

    pub fn timestamp(&self) -> SyncTimestamp {
        self.session.borrow().timestamp
    }

    #[cfg(test)]
    pub(super) fn with_delta<T, E, F>(&self, delta: i64, f: F) -> std::result::Result<T, E>
    where
        F: FnOnce(&Self) -> std::result::Result<T, E>,
    {
        let set = |ts| self.session.borrow_mut().timestamp = SyncTimestamp::from_i64(ts).unwrap();
        let ts = self.timestamp().as_i64();
        set(ts + delta);
        let result = f(&self);
        set(ts);
        result
    }
}

macro_rules! sync_db_method {
    ($name:ident, $sync_name:ident, $type:ident) => {
        sync_db_method!($name, $sync_name, $type, results::$type);
    };
    ($name:ident, $sync_name:ident, $type:ident, $result:ty) => {
        fn $name(&self, params: params::$type) -> DbFuture<$result> {
            let db = self.clone();
            Box::new(self.thread_pool.spawn_handle(lazy(move || {
                future::result(db.$sync_name(params).map_err(Into::into))
            })))
        }
    };
}

impl Db for MysqlDb {
    fn commit(&self) -> DbFuture<()> {
        let db = self.clone();
        Box::new(self.thread_pool.spawn_handle(lazy(move || {
            future::result(db.commit_sync().map_err(Into::into))
        })))
    }

    fn rollback(&self) -> DbFuture<()> {
        let db = self.clone();
        Box::new(self.thread_pool.spawn_handle(lazy(move || {
            future::result(db.rollback_sync().map_err(Into::into))
        })))
    }

    fn box_clone(&self) -> Box<dyn Db> {
        Box::new(self.clone())
    }

    sync_db_method!(lock_for_read, lock_for_read_sync, LockCollection);
    sync_db_method!(lock_for_write, lock_for_write_sync, LockCollection);
    sync_db_method!(
        get_collection_timestamps,
        get_collection_timestamps_sync,
        GetCollectionTimestamps
    );
    sync_db_method!(
        get_collection_timestamp,
        get_collection_timestamp_sync,
        GetCollectionTimestamp
    );
    sync_db_method!(
        get_collection_counts,
        get_collection_counts_sync,
        GetCollectionCounts
    );
    sync_db_method!(
        get_collection_usage,
        get_collection_usage_sync,
        GetCollectionUsage
    );
    sync_db_method!(
        get_storage_timestamp,
        get_storage_timestamp_sync,
        GetStorageTimestamp
    );
    sync_db_method!(get_storage_usage, get_storage_usage_sync, GetStorageUsage);
    sync_db_method!(delete_storage, delete_storage_sync, DeleteStorage);
    sync_db_method!(delete_collection, delete_collection_sync, DeleteCollection);
    sync_db_method!(delete_bsos, delete_bsos_sync, DeleteBsos);
    sync_db_method!(get_bsos, get_bsos_sync, GetBsos);
    sync_db_method!(get_bso_ids, get_bso_ids_sync, GetBsoIds);
    sync_db_method!(post_bsos, post_bsos_sync, PostBsos);
    sync_db_method!(delete_bso, delete_bso_sync, DeleteBso);
    sync_db_method!(get_bso, get_bso_sync, GetBso, Option<results::GetBso>);
    sync_db_method!(
        get_bso_timestamp,
        get_bso_timestamp_sync,
        GetBsoTimestamp,
        results::GetBsoTimestamp
    );
    sync_db_method!(put_bso, put_bso_sync, PutBso);
    sync_db_method!(create_batch, create_batch_sync, CreateBatch);
    sync_db_method!(validate_batch, validate_batch_sync, ValidateBatch);
    sync_db_method!(append_to_batch, append_to_batch_sync, AppendToBatch);
    sync_db_method!(
        get_batch,
        get_batch_sync,
        GetBatch,
        Option<results::GetBatch>
    );
    sync_db_method!(commit_batch, commit_batch_sync, CommitBatch);
}

#[derive(Debug, QueryableByName)]
struct IdResult {
    #[sql_type = "Integer"]
    id: i32,
}

#[allow(dead_code)] // Not really dead, Rust can't see the use above
#[derive(Debug, QueryableByName)]
struct NameResult {
    #[sql_type = "Text"]
    name: String,
}

#[derive(Debug, QueryableByName)]
struct UserCollectionsResult {
    #[sql_type = "Integer"]
    collection_id: i32,
    #[sql_type = "BigInt"]
    modified: i64,
}

#[derive(Debug, QueryableByName)]
struct Count {
    #[sql_type = "BigInt"]
    count: i64,
}

/// Formats a BSO for UPDATEs
#[derive(AsChangeset)]
#[table_name = "bso"]
struct UpdateBSO<'a> {
    pub sortindex: Option<i32>,
    pub payload: Option<&'a str>,
    pub modified: Option<i64>,
    pub expiry: Option<i64>,
}

fn put_bso_as_changeset(bso: &params::PutBso, modified: i64) -> UpdateBSO {
    UpdateBSO {
        sortindex: bso.sortindex,
        expiry: bso.ttl.map(|ttl| modified + (ttl as i64 * 1000)),
        payload: bso.payload.as_ref().map(|payload| &**payload),
        modified: if bso.payload.is_some() || bso.sortindex.is_some() {
            Some(modified)
        } else {
            None
        },
    }
}
