//! The index on disk, in `$XDG_CACHE_HOME/tideline/index.sqlite`.
//!
//! A blob (the content of a file, named by its git OID) is stored once per grammar with the tags parsed from it,
//! however many commits contain it, and the names of the tags are interned. A commit is only a listing of the blobs
//! at its paths. The listing is written in one transaction together with the mark that says it is complete, so a
//! lookup never sees a commit that is half indexed. Nothing is evicted.
//!
//! Every server and job shares the file: WAL mode, a busy timeout and an insert that leaves a stored blob alone make
//! it harmless to write the same blob twice (only a blob that is `unreadable` is replaced, once, by one that is not).
//! A writer connection does the writes and a reader connection the lookups, each behind a mutex, so a lookup never
//! waits for a long insert. A second reader answers a build's question which blobs are stored, a long read that would
//! keep the lookups waiting. The methods are synchronous; async callers run them on the blocking pool.

use std::collections::HashMap;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, ToSql, TransactionBehavior, named_params, params,
};

use super::lang::{Family, Grammar};
use super::rank::{self, Origin};
use super::tags::Tag;
use crate::clock;

/// How long a connection waits for the lock another one holds. The tests hold it on purpose, and a machine that runs
/// many of them at once may take longer than that to get to the point where they let it go.
const BUSY_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 60 } else { 5 });
/// A lookup refreshes the `used_at` of its commit only when it is older than this, so lookups hardly ever write.
const TOUCH_AFTER_SECS: i64 = 3600;

/// Schema migrations; `PRAGMA user_version` is the number already applied.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE blobs (
        id INTEGER PRIMARY KEY,
        oid TEXT NOT NULL,
        grammar TEXT NOT NULL,
        status TEXT NOT NULL,
        size INTEGER NOT NULL,
        UNIQUE (oid, grammar)
    );
    CREATE TABLE names (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE
    );
    CREATE TABLE tags (
        blob_id INTEGER NOT NULL,
        name_id INTEGER NOT NULL,
        kind TEXT NOT NULL,
        is_def INTEGER NOT NULL,
        line INTEGER NOT NULL,
        col INTEGER NOT NULL,
        end_col INTEGER NOT NULL,
        scope_start INTEGER NOT NULL,
        scope_end INTEGER NOT NULL,
        snippet TEXT NOT NULL,
        snippet_col INTEGER NOT NULL
    );
    CREATE INDEX tags_name ON tags (name_id, is_def, blob_id);
    CREATE INDEX tags_blob ON tags (blob_id) WHERE is_def = 1 OR kind = 'implementation';
    CREATE TABLE commits (
        id INTEGER PRIMARY KEY,
        oid TEXT NOT NULL UNIQUE,
        complete INTEGER NOT NULL,
        files INTEGER NOT NULL,
        used_at INTEGER NOT NULL
    );
    CREATE TABLE tree_entries (
        commit_id INTEGER NOT NULL,
        path TEXT NOT NULL,
        blob_id INTEGER NOT NULL,
        family TEXT NOT NULL,
        PRIMARY KEY (commit_id, path)
    ) WITHOUT ROWID;
    CREATE INDEX tree_entries_blob ON tree_entries (commit_id, blob_id, family);
"];

/// The definitions and `implementation` tags of a blob. `tags_blob` holds only these rows (about one tag in eight),
/// which keeps it small, and SQLite uses it only for a condition that implies the one the index was made with.
const SYMBOLS_SQL: &str = "SELECT n.name, t.kind, t.is_def, t.line, t.col, t.end_col, t.scope_start, t.scope_end, t.snippet, t.snippet_col
     FROM tags t JOIN names n ON n.id = t.name_id
     WHERE t.blob_id = ?1 AND (t.is_def = 1 OR t.kind = 'implementation')
     ORDER BY t.scope_start, t.col, t.rowid";

/// What became of a blob when it was indexed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobStatus {
    Parsed,
    /// Over `MAX_BLOB_BYTES`; not parsed.
    TooLarge,
    /// Could not be read.
    Unreadable,
}

impl BlobStatus {
    fn as_str(self) -> &'static str {
        match self {
            BlobStatus::Parsed => "parsed",
            BlobStatus::TooLarge => "too_large",
            BlobStatus::Unreadable => "unreadable",
        }
    }

    /// From the text in the database. A status this version does not know was stored by a newer one, which held the
    /// blob: it counts as `parsed`, so that it is not done again, as a grammar this version does not know is left alone.
    fn from_stored(text: &str) -> BlobStatus {
        [
            BlobStatus::Parsed,
            BlobStatus::TooLarge,
            BlobStatus::Unreadable,
        ]
        .into_iter()
        .find(|status| status.as_str() == text)
        .unwrap_or(BlobStatus::Parsed)
    }
}

/// A blob to store, with the tags `tags::parse` found in it (none unless it was parsed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewBlob {
    pub oid: String,
    pub grammar: Grammar,
    pub status: BlobStatus,
    pub size: u64,
    pub tags: Vec<Tag>,
}

/// The outline of a stored blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSymbols {
    pub status: BlobStatus,
    /// The definitions and the `implementation` tags, sorted by (`scope_start`, `column`).
    pub symbols: Vec<Tag>,
}

/// How close a hit is to the file the lookup came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// The same file.
    File,
    /// A file in the same directory.
    Dir,
    Other,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::File => "file",
            Tier::Dir => "dir",
            Tier::Other => "other",
        }
    }

    /// From the number the SQL of `rank::TIER_SQL` computes.
    fn from_sql(tier: i64) -> Tier {
        match tier {
            0 => Tier::File,
            1 => Tier::Dir,
            _ => Tier::Other,
        }
    }
}

/// A definition or a reference found by a lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub kind: String,
    /// The trimmed line (see `tags::line_snippet`) and the 1-based UTF-16 column of the name inside it. Only
    /// definitions have them; the caller fills those of references from the blobs.
    pub snippet: String,
    pub snippet_column: u32,
    pub tier: Tier,
}

/// The references a lookup found, each with the OID of its blob, and whether there were more than it returned.
pub type References = (Vec<(Hit, String)>, bool);

#[derive(Clone)]
pub struct IndexDb {
    writer: Arc<Mutex<Connection>>,
    reader: Arc<Mutex<Connection>>,
    /// A reader of its own for `stored_blobs`, which a build calls with every blob of a commit. On the shared reader
    /// it would keep the lookups for clicks waiting for as long as it runs: a mutex does not hand over to the thread
    /// waiting for it, so releasing it between chunks of blobs does not help.
    bulk_reader: Arc<Mutex<Connection>>,
}

impl IndexDb {
    pub fn open(path: &Path) -> Result<IndexDb, String> {
        let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        // Create the file 0600 before SQLite does; SQLite gives -wal/-shm the same mode.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| fail(&e))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| fail(&e))?;
        let mut writer = Connection::open(path).map_err(|e| fail(&e))?;
        writer.busy_timeout(BUSY_TIMEOUT).map_err(|e| fail(&e))?;
        writer
            .query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
            .map_err(|e| fail(&e))?;
        // The index can be rebuilt, so a power failure may lose its last transactions but never corrupts it.
        writer
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| fail(&e))?;
        migrate(&mut writer, MIGRATIONS).map_err(|e| fail(&e))?;
        let reader = open_reader(path).map_err(|e| fail(&e))?;
        let bulk_reader = open_reader(path).map_err(|e| fail(&e))?;
        Ok(IndexDb {
            writer: Arc::new(Mutex::new(writer)),
            reader: Arc::new(Mutex::new(reader)),
            bulk_reader: Arc::new(Mutex::new(bulk_reader)),
        })
    }

    /// Every grammar each of `oids` is stored with, and the status of each row. A build skips what it has no reason to
    /// do again (a `.h` file has one grammar, but the same content may be stored under two names). OIDs that are not
    /// stored are left out of the map. It does not use the reader of the lookups, so it never keeps a click waiting.
    pub fn stored_blobs(
        &self,
        oids: &[String],
    ) -> Result<HashMap<String, Vec<(Grammar, BlobStatus)>>, String> {
        let conn = lock(&self.bulk_reader);
        let mut query = conn
            .prepare_cached("SELECT grammar, status FROM blobs WHERE oid = ?1")
            .map_err(db_err)?;
        let mut stored = HashMap::new();
        for oid in oids {
            let rows = query
                .query_map([oid], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(db_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_err)?;
            let mut held = Vec::new();
            for (id, status) in rows {
                // A grammar this version does not know was stored by a newer one.
                let Some(grammar) = Grammar::from_id(&id) else {
                    continue;
                };
                held.push((grammar, BlobStatus::from_stored(&status)));
            }
            if !held.is_empty() {
                held.sort_by_key(|&(grammar, _)| grammar);
                stored.insert(oid.clone(), held);
            }
        }
        Ok(stored)
    }

    /// The size of each of `oids` that is stored (under any grammar); the others are left out of the map.
    pub fn blob_sizes(&self, oids: &[String]) -> Result<HashMap<String, u64>, String> {
        let conn = lock(&self.reader);
        let mut query = conn
            .prepare_cached("SELECT max(size) FROM blobs WHERE oid = ?1")
            .map_err(db_err)?;
        let mut sizes = HashMap::new();
        for oid in oids {
            let size: Option<i64> = query.query_row([oid], |row| row.get(0)).map_err(db_err)?;
            if let Some(size) = size {
                sizes.insert(oid.clone(), size as u64);
            }
        }
        Ok(sizes)
    }

    /// Stores blobs and their tags in one transaction. A blob already stored as (OID, grammar) is left as it is, and
    /// so are its tags, so two jobs, or two servers, may store the same blob; except that a row that says `unreadable`
    /// is replaced by one that does not, with the tags that go with it. `unreadable` is a failure that may not be
    /// there next time (a blob that could not be fetched, a parse that took too long), so it does not stay for ever.
    pub fn insert_blobs(&self, blobs: Vec<NewBlob>) -> Result<(), String> {
        let mut conn = lock(&self.writer);
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        {
            // Returns the id of the row if this stored the blob or replaced an `unreadable` row, else nothing.
            let mut insert_blob = tx
                .prepare_cached(
                    "INSERT INTO blobs (oid, grammar, status, size) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (oid, grammar) DO UPDATE SET status = excluded.status, size = excluded.size
                     WHERE blobs.status = 'unreadable' AND excluded.status <> 'unreadable'
                     RETURNING id",
                )
                .map_err(db_err)?;
            let mut insert_tag = tx
                .prepare_cached(
                    "INSERT INTO tags
                        (blob_id, name_id, kind, is_def, line, col, end_col, scope_start, scope_end, snippet, snippet_col)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )
                .map_err(db_err)?;
            // Names repeat a lot within a batch.
            let mut name_ids: HashMap<String, i64> = HashMap::new();
            for blob in blobs {
                let blob_id: Option<i64> = insert_blob
                    .query_row(
                        params![
                            blob.oid,
                            blob.grammar.id(),
                            blob.status.as_str(),
                            blob.size as i64
                        ],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db_err)?;
                let Some(blob_id) = blob_id else {
                    continue;
                };
                for tag in blob.tags {
                    let name_id = match name_ids.get(&tag.name) {
                        Some(&id) => id,
                        None => {
                            let id = intern(&tx, &tag.name).map_err(db_err)?;
                            name_ids.insert(tag.name, id);
                            id
                        }
                    };
                    insert_tag
                        .execute(params![
                            blob_id,
                            name_id,
                            tag.kind,
                            tag.is_def,
                            tag.line,
                            tag.column,
                            tag.end_column,
                            tag.scope_start,
                            tag.scope_end,
                            tag.snippet,
                            tag.snippet_column
                        ])
                        .map_err(db_err)?;
                }
            }
        }
        tx.commit().map_err(db_err)
    }

    /// The outline of a stored blob; None when it is not stored.
    pub fn blob_symbols(
        &self,
        oid: &str,
        grammar: Grammar,
    ) -> Result<Option<StoredSymbols>, String> {
        let conn = lock(&self.reader);
        let blob = conn
            .query_row(
                "SELECT id, status FROM blobs WHERE oid = ?1 AND grammar = ?2",
                params![oid, grammar.id()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((blob_id, status)) = blob else {
            return Ok(None);
        };
        let status = BlobStatus::from_stored(&status);
        let mut query = conn.prepare_cached(SYMBOLS_SQL).map_err(db_err)?;
        let symbols = query
            .query_map([blob_id], |row| {
                Ok(Tag {
                    name: row.get(0)?,
                    kind: row.get(1)?,
                    is_def: row.get(2)?,
                    line: row.get(3)?,
                    column: row.get(4)?,
                    end_column: row.get(5)?,
                    scope_start: row.get(6)?,
                    scope_end: row.get(7)?,
                    snippet: row.get(8)?,
                    snippet_column: row.get(9)?,
                })
            })
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(Some(StoredSymbols { status, symbols }))
    }

    /// Whether the commit has been saved with `save_commit`.
    pub fn commit_complete(&self, commit: &str) -> Result<bool, String> {
        let conn = lock(&self.reader);
        Ok(complete_commit(&conn, commit)?.is_some())
    }

    /// The grammar the complete commit holds the file at `path` under; None when it does not list the file. A `.h`
    /// file is the only one whose grammar its name does not give.
    pub fn path_grammar(&self, commit: &str, path: &str) -> Result<Option<Grammar>, String> {
        let conn = lock(&self.reader);
        let Some((commit_id, _)) = complete_commit(&conn, commit)? else {
            return Ok(None);
        };
        let id: Option<String> = conn
            .query_row(
                "SELECT b.grammar FROM tree_entries te JOIN blobs b ON b.id = te.blob_id
                 WHERE te.commit_id = ?1 AND te.path = ?2",
                params![commit_id, path],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(id.as_deref().and_then(Grammar::from_id))
    }

    /// Records the files of a commit, with the OID and grammar of the blob at each path, and marks the commit
    /// complete, in one transaction. Every blob must be stored; if one is not, nothing is recorded.
    pub fn save_commit(
        &self,
        commit: &str,
        files: &[(String, String, Grammar)],
    ) -> Result<(), String> {
        let mut conn = lock(&self.writer);
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let now = clock::unix_now() as i64;
        tx.execute(
            "INSERT INTO commits (oid, complete, files, used_at) VALUES (?1, 0, 0, ?2)
             ON CONFLICT (oid) DO NOTHING",
            params![commit, now],
        )
        .map_err(db_err)?;
        let commit_id: i64 = tx
            .query_row("SELECT id FROM commits WHERE oid = ?1", [commit], |row| {
                row.get(0)
            })
            .map_err(db_err)?;
        tx.execute("DELETE FROM tree_entries WHERE commit_id = ?1", [commit_id])
            .map_err(db_err)?;
        {
            let mut insert = tx
                .prepare_cached(
                    "INSERT INTO tree_entries (commit_id, path, blob_id, family)
                     SELECT ?1, ?2, id, ?3 FROM blobs WHERE oid = ?4 AND grammar = ?5",
                )
                .map_err(db_err)?;
            for (path, oid, grammar) in files {
                let added = insert
                    .execute(params![
                        commit_id,
                        path,
                        grammar.family().id(),
                        oid,
                        grammar.id()
                    ])
                    .map_err(db_err)?;
                if added == 0 {
                    return Err(format!(
                        "index database: blob {oid} ({}) of {path} is not stored",
                        grammar.id()
                    ));
                }
            }
        }
        tx.execute(
            "UPDATE commits SET complete = 1, files = ?2, used_at = ?3 WHERE id = ?1",
            params![commit_id, files.len() as i64, now],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// The definitions of `name` in the files of `family` in the commit, the nearest to `from_path`, the file of the
    /// click, first (see `rank`); at most 200, which are the nearest of all. None when the commit is not completely
    /// indexed.
    ///
    /// Constructors are left out (they remain symbols of their file): they have the name of their class, so the class
    /// would otherwise never be the only definition of `new Foo()`.
    pub fn definitions(
        &self,
        commit: &str,
        family: Family,
        name: &str,
        from_path: &str,
    ) -> Result<Option<Vec<Hit>>, String> {
        let conn = lock(&self.reader);
        let Some((commit_id, used_at)) = complete_commit(&conn, commit)? else {
            return Ok(None);
        };
        let origin = Origin::new(from_path);
        let ancestors = origin.ancestors();
        let sql = format!(
            "SELECT te.path, t.line, t.col, t.end_col, t.kind, t.snippet, t.snippet_col, {tier} AS tier
             FROM names n
             JOIN tags t ON t.name_id = n.id AND t.is_def = 1 AND t.kind <> 'constructor'
             JOIN tree_entries te ON te.commit_id = :commit AND te.blob_id = t.blob_id AND te.family = :family
             WHERE n.name = :name
             ORDER BY {order}
             LIMIT {limit}",
            tier = rank::TIER_SQL,
            order = rank::nearest_first_sql(&ancestors),
            limit = rank::MAX_DEFINITIONS
        );
        let family = family.id();
        let mut params: Vec<(&str, &dyn ToSql)> = vec![
            (":commit", &commit_id),
            (":family", &family),
            (":name", &name),
            (":from", &origin.path),
            (":dir", &origin.dir),
        ];
        params.extend(
            ancestors
                .iter()
                .map(|(param, dir)| (param.as_str(), dir as &dyn ToSql)),
        );
        // Not `prepare_cached`: the text depends on how deep the file of the click is (`rank::nearest_first_sql`), up to
        // 65 different statements, which would push the statements that stay the same out of the cache.
        let mut query = conn.prepare(&sql).map_err(db_err)?;
        let hits = query
            .query_map(params.as_slice(), |row| {
                Ok(Hit {
                    path: row.get(0)?,
                    line: row.get(1)?,
                    column: row.get(2)?,
                    end_column: row.get(3)?,
                    kind: row.get(4)?,
                    snippet: row.get(5)?,
                    snippet_column: row.get(6)?,
                    tier: Tier::from_sql(row.get(7)?),
                })
            })
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        drop(query);
        drop(conn);
        self.touch(commit_id, used_at);
        Ok(Some(hits))
    }

    /// The references to `name` in the files of `family` in the commit: the same file first, then its directory,
    /// then by path and line; at most 1000, and whether there are more. Each comes with the OID of its blob, from
    /// which the caller reads the snippet. None when the commit is not completely indexed.
    pub fn references(
        &self,
        commit: &str,
        family: Family,
        name: &str,
        from_path: &str,
    ) -> Result<Option<References>, String> {
        let conn = lock(&self.reader);
        let Some((commit_id, used_at)) = complete_commit(&conn, commit)? else {
            return Ok(None);
        };
        let origin = Origin::new(from_path);
        let sql = format!(
            "SELECT te.path, t.line, t.col, t.end_col, t.kind, t.blob_id, {tier} AS tier
             FROM names n
             JOIN tags t ON t.name_id = n.id AND t.is_def = 0
             JOIN tree_entries te ON te.commit_id = :commit AND te.blob_id = t.blob_id AND te.family = :family
             WHERE n.name = :name
             ORDER BY tier, te.path, t.line, t.col
             LIMIT {limit}",
            tier = rank::TIER_SQL,
            limit = rank::MAX_REFERENCES + 1
        );
        let mut query = conn.prepare_cached(&sql).map_err(db_err)?;
        let rows = query
            .query_map(
                named_params! {
                    ":commit": commit_id,
                    ":family": family.id(),
                    ":name": name,
                    ":from": origin.path,
                    ":dir": origin.dir,
                },
                |row| {
                    let hit = Hit {
                        path: row.get(0)?,
                        line: row.get(1)?,
                        column: row.get(2)?,
                        end_column: row.get(3)?,
                        kind: row.get(4)?,
                        snippet: String::new(),
                        snippet_column: 0,
                        tier: Tier::from_sql(row.get(6)?),
                    };
                    Ok((hit, row.get::<_, i64>(5)?))
                },
            )
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        let truncated = rows.len() > rank::MAX_REFERENCES;
        // The OIDs of the blobs are only looked up for the rows that are returned.
        let mut oid_of = conn
            .prepare_cached("SELECT oid FROM blobs WHERE id = ?1")
            .map_err(db_err)?;
        let mut oids: HashMap<i64, String> = HashMap::new();
        let mut hits = Vec::with_capacity(rows.len().min(rank::MAX_REFERENCES));
        for (hit, blob_id) in rows.into_iter().take(rank::MAX_REFERENCES) {
            let oid = match oids.get(&blob_id) {
                Some(oid) => oid.clone(),
                None => {
                    let oid: String = oid_of
                        .query_row([blob_id], |row| row.get(0))
                        .map_err(db_err)?;
                    oids.insert(blob_id, oid.clone());
                    oid
                }
            };
            hits.push((hit, oid));
        }
        drop(oid_of);
        drop(query);
        drop(conn);
        self.touch(commit_id, used_at);
        Ok(Some((hits, truncated)))
    }

    /// Notes that a lookup used the commit, for a future cleanup of what has not been used for long. Best effort and
    /// never in the way of the lookup: skipped when the note is recent and when the writer is busy.
    fn touch(&self, commit_id: i64, used_at: i64) {
        let now = clock::unix_now() as i64;
        if now.saturating_sub(used_at) < TOUCH_AFTER_SECS {
            return;
        }
        let Ok(conn) = self.writer.try_lock() else {
            return;
        };
        // Don't wait for a server that is writing.
        let _ = conn.busy_timeout(Duration::ZERO);
        let _ = conn.execute(
            "UPDATE commits SET used_at = ?2 WHERE id = ?1 AND used_at < ?2",
            params![commit_id, now],
        );
        let _ = conn.busy_timeout(BUSY_TIMEOUT);
    }
}

/// The id and `used_at` of the commit, if it is complete.
fn complete_commit(conn: &Connection, commit: &str) -> Result<Option<(i64, i64)>, String> {
    conn.query_row(
        "SELECT id, used_at FROM commits WHERE oid = ?1 AND complete = 1",
        [commit],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(db_err)
}

/// The id of a name, which is added if it is new.
fn intern(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    let known = conn
        .prepare_cached("SELECT id FROM names WHERE name = ?1")?
        .query_row([name], |row| row.get(0))
        .optional()?;
    if let Some(id) = known {
        return Ok(id);
    }
    conn.prepare_cached("INSERT INTO names (name) VALUES (?1)")?
        .execute([name])?;
    Ok(conn.last_insert_rowid())
}

fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let reader = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    reader.busy_timeout(BUSY_TIMEOUT)?;
    Ok(reader)
}

fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    // A panic while the lock was held leaves nothing half done: unwinding drops the open `Transaction`, which rolls
    // back when it is dropped without a commit.
    conn.lock().unwrap_or_else(PoisonError::into_inner)
}

fn db_err(e: rusqlite::Error) -> String {
    format!("index database: {e}")
}

/// Applies pending migrations. `BEGIN IMMEDIATE` serializes concurrent servers, and the
/// version is re-read inside the transaction so each migration runs exactly once.
fn migrate(conn: &mut Connection, migrations: &[&str]) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if applied < 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(0, applied));
    }
    // A newer gh-tideline may have migrated further; leave its schema alone.
    if applied >= migrations.len() as i64 {
        return Ok(());
    }
    for sql in &migrations[applied as usize..] {
        tx.execute_batch(sql)?;
    }
    tx.pragma_update(None, "user_version", migrations.len() as i64)?;
    tx.commit()
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;
    use crate::index::tags;

    /// A database in a directory of its own, which is removed when the test ends.
    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
        db: IndexDb,
    }

    impl TempDb {
        fn new(test: &str) -> TempDb {
            let dir =
                std::env::temp_dir().join(format!("tideline-index-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("index.sqlite");
            let db = IndexDb::open(&path).unwrap();
            TempDb { dir, path, db }
        }

        /// A connection of its own to the file, like another server's.
        fn side(&self) -> Connection {
            Connection::open(&self.path).unwrap()
        }

        fn count(&self, sql: &str) -> i64 {
            self.side().query_row(sql, [], |row| row.get(0)).unwrap()
        }
    }

    impl Deref for TempDb {
        type Target = IndexDb;

        fn deref(&self) -> &IndexDb {
            &self.db
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn oid(n: u32) -> String {
        format!("{n:040x}")
    }

    fn commit(n: u32) -> String {
        oid(1_000_000 + n)
    }

    /// A definition `fn <name>() {` on one line, or a reference without a snippet.
    fn tag(name: &str, kind: &str, is_def: bool, line: u32, column: u32) -> Tag {
        Tag {
            name: name.to_string(),
            kind: kind.to_string(),
            is_def,
            line,
            column,
            end_column: column + name.len() as u32,
            scope_start: line,
            scope_end: if is_def { line + 2 } else { line },
            snippet: if is_def {
                format!("fn {name}() {{")
            } else {
                String::new()
            },
            snippet_column: if is_def { 4 } else { 0 },
        }
    }

    fn def(name: &str, line: u32, column: u32) -> Tag {
        tag(name, "function", true, line, column)
    }

    fn call(name: &str, line: u32, column: u32) -> Tag {
        tag(name, "call", false, line, column)
    }

    fn blob(n: u32, grammar: Grammar, tags: Vec<Tag>) -> NewBlob {
        NewBlob {
            oid: oid(n),
            grammar,
            status: BlobStatus::Parsed,
            size: 100,
            tags,
        }
    }

    fn entry(path: &str, n: u32, grammar: Grammar) -> (String, String, Grammar) {
        (path.to_string(), oid(n), grammar)
    }

    /// Stores the blobs and a commit that lists them at the paths, all Rust.
    fn rust_commit(db: &IndexDb, commit: &str, files: Vec<(&str, Vec<Tag>)>) {
        let mut blobs = Vec::new();
        let mut entries = Vec::new();
        for (i, (path, tags)) in files.into_iter().enumerate() {
            blobs.push(blob(i as u32 + 1, Grammar::Rust, tags));
            entries.push(entry(path, i as u32 + 1, Grammar::Rust));
        }
        db.insert_blobs(blobs).unwrap();
        db.save_commit(commit, &entries).unwrap();
    }

    fn places(hits: &[Hit]) -> Vec<(&str, u32)> {
        hits.iter().map(|h| (h.path.as_str(), h.line)).collect()
    }

    #[test]
    fn the_file_is_private_and_reopening_keeps_the_data() {
        let fx = TempDb::new("private");
        rust_commit(&fx, &commit(1), vec![("a.rs", vec![def("f", 1, 4)])]);

        // The -wal and -shm files SQLite keeps beside it are private too.
        let mut files = 0;
        for file in std::fs::read_dir(&fx.dir).unwrap() {
            let mode = file.unwrap().metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            files += 1;
        }
        assert!(files >= 1);
        assert_eq!(fx.count("PRAGMA user_version"), MIGRATIONS.len() as i64);

        // A second server opening the file keeps the data and runs no migration.
        let again = IndexDb::open(&fx.path).unwrap();
        assert!(again.commit_complete(&commit(1)).unwrap());
        assert_eq!(
            again
                .blob_symbols(&oid(1), Grammar::Rust)
                .unwrap()
                .unwrap()
                .symbols,
            vec![def("f", 1, 4)]
        );
        assert_eq!(fx.count("PRAGMA user_version"), MIGRATIONS.len() as i64);
    }

    #[test]
    fn a_file_from_a_newer_version_is_left_alone() {
        let fx = TempDb::new("newer");
        fx.side()
            .execute_batch("CREATE TABLE from_the_future (x); PRAGMA user_version = 99;")
            .unwrap();
        let again = IndexDb::open(&fx.path).unwrap();
        assert_eq!(fx.count("PRAGMA user_version"), 99);
        again
            .insert_blobs(vec![blob(1, Grammar::Rust, vec![def("f", 1, 4)])])
            .unwrap();
        assert!(
            again
                .blob_symbols(&oid(1), Grammar::Rust)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn migrations_run_once_and_only_the_pending_ones() {
        let first = "CREATE TABLE a (x)";
        let second = "CREATE TABLE b (y)";
        let version = |conn: &Connection| -> i64 {
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap()
        };
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, &[first]).unwrap();
        assert_eq!(version(&conn), 1);
        // Creating `a` again would fail.
        migrate(&mut conn, &[first, second]).unwrap();
        assert_eq!(version(&conn), 2);
        migrate(&mut conn, &[first, second]).unwrap();
        assert_eq!(version(&conn), 2);
        // An older program leaves what a newer one made.
        migrate(&mut conn, &[first]).unwrap();
        assert_eq!(version(&conn), 2);
        // A migration that fails undoes the ones before it.
        let err = migrate(&mut conn, &[first, second, "CREATE TABLE c (z)", "NOT SQL"]);
        assert!(err.is_err());
        assert_eq!(version(&conn), 2);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'c'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn a_blob_is_stored_once_per_grammar_and_the_first_copy_stays() {
        let fx = TempDb::new("once");
        fx.insert_blobs(vec![blob(1, Grammar::C, vec![def("f", 1, 5)])])
            .unwrap();
        // The blob again with other tags, twice in one batch, next to a new blob.
        fx.insert_blobs(vec![
            blob(1, Grammar::C, vec![def("g", 9, 9), call("f", 2, 2)]),
            blob(1, Grammar::C, vec![]),
            blob(2, Grammar::C, vec![]),
        ])
        .unwrap();
        assert_eq!(
            fx.blob_symbols(&oid(1), Grammar::C)
                .unwrap()
                .unwrap()
                .symbols,
            vec![def("f", 1, 5)]
        );
        assert_eq!(fx.count("SELECT count(*) FROM tags"), 1);
        assert_eq!(fx.count("SELECT count(*) FROM blobs"), 2);

        // The same content under another grammar (a `.h` file next to a `.cpp` one) is another blob.
        fx.insert_blobs(vec![blob(1, Grammar::Cpp, vec![def("h", 3, 3)])])
            .unwrap();
        assert_eq!(
            fx.blob_symbols(&oid(1), Grammar::Cpp)
                .unwrap()
                .unwrap()
                .symbols,
            vec![def("h", 3, 3)]
        );
        assert_eq!(
            fx.blob_symbols(&oid(1), Grammar::C)
                .unwrap()
                .unwrap()
                .symbols,
            vec![def("f", 1, 5)]
        );
        assert_eq!(fx.blob_symbols(&oid(1), Grammar::Go).unwrap(), None);
        assert_eq!(fx.blob_symbols(&oid(3), Grammar::C).unwrap(), None);

        let stored = fx.stored_blobs(&[oid(1), oid(2), oid(3)]).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(
            stored[&oid(1)],
            [
                (Grammar::C, BlobStatus::Parsed),
                (Grammar::Cpp, BlobStatus::Parsed)
            ]
        );
        assert_eq!(stored[&oid(2)], [(Grammar::C, BlobStatus::Parsed)]);
        assert!(fx.stored_blobs(&[]).unwrap().is_empty());
    }

    fn unreadable(n: u32, grammar: Grammar) -> NewBlob {
        NewBlob {
            status: BlobStatus::Unreadable,
            size: 0,
            ..blob(n, grammar, Vec::new())
        }
    }

    #[test]
    fn an_unreadable_blob_is_replaced_by_one_that_was_read_and_nothing_else_is() {
        let fx = TempDb::new("unreadable");
        let status = |n: u32, grammar: Grammar| {
            fx.blob_symbols(&oid(n), grammar)
                .unwrap()
                .map(|stored| (stored.status, stored.symbols))
        };
        let size_of =
            |n: u32| fx.count(&format!("SELECT size FROM blobs WHERE oid = '{}'", oid(n)));

        // A blob that could not be read, twice (nothing changes), and then read.
        fx.insert_blobs(vec![unreadable(1, Grammar::Rust)]).unwrap();
        fx.insert_blobs(vec![unreadable(1, Grammar::Rust)]).unwrap();
        assert_eq!(
            status(1, Grammar::Rust),
            Some((BlobStatus::Unreadable, vec![]))
        );
        // A commit that lists it, as a build leaves it when the blob stays unreadable.
        fx.save_commit(&commit(1), &[entry("a.rs", 1, Grammar::Rust)])
            .unwrap();

        fx.insert_blobs(vec![blob(1, Grammar::Rust, vec![def("f", 1, 4)])])
            .unwrap();
        assert_eq!(
            status(1, Grammar::Rust),
            Some((BlobStatus::Parsed, vec![def("f", 1, 4)]))
        );
        assert_eq!(
            (size_of(1), fx.count("SELECT count(*) FROM blobs")),
            (100, 1)
        );
        // The row is the same one: the commit that listed it finds the tags.
        let hits = fx
            .definitions(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap()
            .unwrap();
        assert_eq!(places(&hits), [("a.rs", 1)]);

        // What was read stays as it was: not replaced by an unreadable row, nor by another reading.
        fx.insert_blobs(vec![
            unreadable(1, Grammar::Rust),
            blob(1, Grammar::Rust, vec![def("g", 7, 7), call("f", 2, 2)]),
        ])
        .unwrap();
        assert_eq!(
            status(1, Grammar::Rust),
            Some((BlobStatus::Parsed, vec![def("f", 1, 4)]))
        );
        assert_eq!(fx.count("SELECT count(*) FROM tags"), 1);

        // Too large is an answer too. The size is the one that was found out.
        fx.insert_blobs(vec![unreadable(2, Grammar::Go)]).unwrap();
        fx.insert_blobs(vec![NewBlob {
            status: BlobStatus::TooLarge,
            size: 5 << 20,
            ..blob(2, Grammar::Go, Vec::new())
        }])
        .unwrap();
        assert_eq!(status(2, Grammar::Go), Some((BlobStatus::TooLarge, vec![])));
        assert_eq!(size_of(2), 5 << 20);

        // Both in one batch, whichever comes first.
        fx.insert_blobs(vec![
            unreadable(3, Grammar::C),
            blob(3, Grammar::C, vec![def("h", 3, 3)]),
            blob(4, Grammar::C, vec![def("i", 4, 4)]),
            unreadable(4, Grammar::C),
        ])
        .unwrap();
        assert_eq!(
            status(3, Grammar::C),
            Some((BlobStatus::Parsed, vec![def("h", 3, 3)]))
        );
        assert_eq!(
            status(4, Grammar::C),
            Some((BlobStatus::Parsed, vec![def("i", 4, 4)]))
        );
        // Each grammar of a blob is a row of its own.
        fx.insert_blobs(vec![unreadable(3, Grammar::Cpp)]).unwrap();
        assert_eq!(
            status(3, Grammar::Cpp),
            Some((BlobStatus::Unreadable, vec![]))
        );
        assert_eq!(
            status(3, Grammar::C),
            Some((BlobStatus::Parsed, vec![def("h", 3, 3)]))
        );
    }

    #[test]
    fn two_servers_replacing_the_same_unreadable_blob_store_its_tags_once() {
        let fx = TempDb::new("unreadable-twice");
        let other_server = IndexDb::open(&fx.path).unwrap();
        fx.insert_blobs((0..40).map(|n| unreadable(n, Grammar::Rust)).collect())
            .unwrap();

        std::thread::scope(|scope| {
            for db in [fx.db.clone(), other_server] {
                scope.spawn(move || {
                    for round in 0..10u32 {
                        let blobs = (0..40)
                            .map(|n| blob(n, Grammar::Rust, vec![def(&format!("f{n}"), n + 1, 4)]))
                            .collect();
                        db.insert_blobs(blobs).unwrap();
                        // Still unreadable in the eyes of a slower build that read nothing.
                        db.insert_blobs(vec![unreadable(round, Grammar::Rust)])
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(fx.count("SELECT count(*) FROM blobs"), 40);
        assert_eq!(
            fx.count("SELECT count(*) FROM blobs WHERE status = 'parsed'"),
            40
        );
        assert_eq!(fx.count("SELECT count(*) FROM tags"), 40);
    }

    #[test]
    fn what_is_stored_says_how_each_row_went_and_how_big_the_blob_is() {
        let fx = TempDb::new("stored");
        fx.insert_blobs(vec![
            blob(1, Grammar::C, vec![]),
            NewBlob {
                size: 7,
                ..unreadable(1, Grammar::Cpp)
            },
            NewBlob {
                status: BlobStatus::TooLarge,
                size: 5 << 20,
                ..blob(2, Grammar::Go, vec![])
            },
        ])
        .unwrap();
        let stored = fx.stored_blobs(&[oid(1), oid(2), oid(3)]).unwrap();
        assert_eq!(
            stored,
            HashMap::from([
                (
                    oid(1),
                    vec![
                        (Grammar::C, BlobStatus::Parsed),
                        (Grammar::Cpp, BlobStatus::Unreadable)
                    ]
                ),
                (oid(2), vec![(Grammar::Go, BlobStatus::TooLarge)]),
            ])
        );

        // A size is the largest of the rows (an unreadable one may not know it), and a blob that is not stored has none.
        let sizes = fx.blob_sizes(&[oid(1), oid(2), oid(3)]).unwrap();
        assert_eq!(sizes, HashMap::from([(oid(1), 100), (oid(2), 5 << 20)]));
        assert!(fx.blob_sizes(&[]).unwrap().is_empty());

        // A status this version does not know was stored by a newer one, which held the blob: the row counts as held
        // and parsed, instead of failing every build and outline that meets it. A grammar it does not know is left out.
        fx.side()
            .execute_batch(&format!(
                "INSERT INTO blobs (oid, grammar, status, size) VALUES ('{0}', 'rust', 'surprise', 1);
                 INSERT INTO blobs (oid, grammar, status, size) VALUES ('{0}', 'cobol', 'parsed', 1);",
                oid(9)
            ))
            .unwrap();
        assert_eq!(
            fx.stored_blobs(&[oid(9)]).unwrap(),
            HashMap::from([(oid(9), vec![(Grammar::Rust, BlobStatus::Parsed)])])
        );
        assert_eq!(
            fx.blob_symbols(&oid(9), Grammar::Rust).unwrap(),
            Some(StoredSymbols {
                status: BlobStatus::Parsed,
                symbols: vec![]
            })
        );
    }

    #[test]
    fn symbols_are_the_definitions_and_the_implementations_in_source_order() {
        let fx = TempDb::new("symbols");
        let mut implementation = tag("Foo", "implementation", false, 10, 6);
        implementation.scope_end = 40;
        implementation.snippet = "impl Foo {".to_string();
        implementation.snippet_column = 6;
        let late = def("late", 50, 8);
        let early = def("early", 3, 4);
        let inner = def("inner", 12, 8);
        let a = def("same_line_a", 30, 8);
        let b = def("same_line_b", 30, 20);
        fx.insert_blobs(vec![blob(
            1,
            Grammar::Rust,
            vec![
                call("helper", 2, 5),
                late.clone(),
                implementation.clone(),
                early.clone(),
                inner.clone(),
                call("early", 15, 9),
                b.clone(),
                a.clone(),
            ],
        )])
        .unwrap();
        let stored = fx.blob_symbols(&oid(1), Grammar::Rust).unwrap().unwrap();
        assert_eq!(stored.status, BlobStatus::Parsed);
        assert_eq!(stored.symbols, [early, implementation, inner, a, b, late]);
    }

    #[test]
    fn the_symbols_of_a_blob_are_read_through_the_small_index() {
        let fx = TempDb::new("symbols-plan");
        let conn = fx.side();
        let mut explain = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {SYMBOLS_SQL}"))
            .unwrap();
        let plan: Vec<String> = explain
            .query_map([1], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // Without the index SQLite would read every tag of every blob.
        assert!(
            plan.iter()
                .any(|step| step.starts_with("SEARCH t USING INDEX tags_blob")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("SCAN")),
            "{plan:?}"
        );
    }

    #[test]
    fn blobs_that_were_not_parsed_have_a_status_and_no_symbols() {
        let fx = TempDb::new("status");
        let statuses = [
            BlobStatus::Parsed,
            BlobStatus::TooLarge,
            BlobStatus::Unreadable,
        ];
        for (n, status) in statuses.into_iter().enumerate() {
            assert_eq!(BlobStatus::from_stored(status.as_str()), status);
            fx.insert_blobs(vec![NewBlob {
                oid: oid(n as u32),
                grammar: Grammar::Go,
                status,
                size: 5 << 20,
                tags: vec![],
            }])
            .unwrap();
            assert_eq!(
                fx.blob_symbols(&oid(n as u32), Grammar::Go).unwrap(),
                Some(StoredSymbols {
                    status,
                    symbols: vec![]
                })
            );
        }
        assert_eq!(BlobStatus::from_stored("surprise"), BlobStatus::Parsed);
        let oids: Vec<_> = (0..3).map(oid).collect();
        assert_eq!(fx.stored_blobs(&oids).unwrap().len(), 3);
        let size = format!("SELECT size FROM blobs WHERE oid = '{}'", oid(1));
        assert_eq!(fx.count(&size), 5 << 20);
    }

    #[test]
    fn a_commit_is_complete_once_it_is_saved() {
        let fx = TempDb::new("complete");
        fx.insert_blobs(vec![blob(
            1,
            Grammar::Rust,
            vec![def("f", 1, 4), call("f", 5, 5)],
        )])
        .unwrap();
        let lookup = |commit: &str| fx.definitions(commit, Family::Rust, "f", "a.rs").unwrap();
        let c = commit(1);
        assert!(!fx.commit_complete(&c).unwrap());
        assert_eq!(lookup(&c), None);
        assert_eq!(fx.references(&c, Family::Rust, "f", "a.rs").unwrap(), None);

        fx.save_commit(&c, &[entry("a.rs", 1, Grammar::Rust)])
            .unwrap();
        assert!(fx.commit_complete(&c).unwrap());
        assert_eq!(places(&lookup(&c).unwrap()), [("a.rs", 1)]);

        // A commit that another version left half written is not complete until it is saved.
        fx.side()
            .execute(
                "INSERT INTO commits (oid, complete, files, used_at) VALUES (?1, 0, 0, 0)",
                [commit(2)],
            )
            .unwrap();
        assert!(!fx.commit_complete(&commit(2)).unwrap());
        assert_eq!(lookup(&commit(2)), None);
        fx.save_commit(&commit(2), &[entry("b.rs", 1, Grammar::Rust)])
            .unwrap();
        assert_eq!(places(&lookup(&commit(2)).unwrap()), [("b.rs", 1)]);

        // Saving a commit again replaces its listing.
        fx.save_commit(
            &c,
            &[
                entry("c.rs", 1, Grammar::Rust),
                entry("d.rs", 1, Grammar::Rust),
            ],
        )
        .unwrap();
        assert_eq!(places(&lookup(&c).unwrap()), [("c.rs", 1), ("d.rs", 1)]);
        assert_eq!(
            fx.count(&format!("SELECT files FROM commits WHERE oid = '{c}'")),
            2
        );
        assert_eq!(fx.count("SELECT count(*) FROM commits"), 2);
    }

    #[test]
    fn saving_a_commit_with_a_blob_that_is_not_stored_records_nothing() {
        let fx = TempDb::new("unknown");
        fx.insert_blobs(vec![blob(1, Grammar::Rust, vec![def("f", 1, 4)])])
            .unwrap();
        let c = commit(1);
        let bad = [
            // Never stored.
            vec![
                entry("a.rs", 1, Grammar::Rust),
                entry("b.rs", 2, Grammar::Rust),
            ],
            // Stored, but with another grammar.
            vec![entry("a.rs", 1, Grammar::Go)],
            // The same path twice.
            vec![
                entry("a.rs", 1, Grammar::Rust),
                entry("a.rs", 1, Grammar::Rust),
            ],
        ];
        for files in &bad {
            assert!(fx.save_commit(&c, files).is_err());
            assert!(!fx.commit_complete(&c).unwrap());
            assert_eq!(fx.count("SELECT count(*) FROM commits"), 0);
            assert_eq!(fx.count("SELECT count(*) FROM tree_entries"), 0);
        }
        let err = fx.save_commit(&c, &bad[0]).unwrap_err();
        assert!(err.contains(&oid(2)) && err.contains("b.rs"), "{err}");

        fx.save_commit(&c, &[entry("a.rs", 1, Grammar::Rust)])
            .unwrap();
        assert!(fx.commit_complete(&c).unwrap());
    }

    #[test]
    fn a_file_of_a_commit_is_known_by_the_grammar_it_is_held_under() {
        let fx = TempDb::new("path-grammar");
        fx.insert_blobs(vec![
            blob(1, Grammar::Cpp, Vec::new()),
            blob(2, Grammar::C, Vec::new()),
        ])
        .unwrap();
        let c = commit(1);
        // Nothing is known about the files of a commit that is not saved.
        assert_eq!(fx.path_grammar(&c, "include/a.h").unwrap(), None);

        fx.save_commit(
            &c,
            &[
                entry("include/a.h", 1, Grammar::Cpp),
                entry("include/b.h", 2, Grammar::C),
            ],
        )
        .unwrap();
        let grammar = |commit: &str, path: &str| fx.path_grammar(commit, path).unwrap();
        assert_eq!(grammar(&c, "include/a.h"), Some(Grammar::Cpp));
        assert_eq!(grammar(&c, "include/b.h"), Some(Grammar::C));
        // A file the commit does not list, and a commit that is not saved.
        assert_eq!(grammar(&c, "include/c.h"), None);
        assert_eq!(grammar(&commit(2), "include/a.h"), None);
    }

    #[test]
    fn definitions_are_ranked_by_closeness_to_the_file_of_the_click() {
        let fx = TempDb::new("ranking");
        rust_commit(
            &fx,
            &commit(1),
            vec![
                (
                    "src/a/b/c.rs",
                    vec![
                        call("parse", 3, 9),
                        def("parse", 30, 8),
                        def("other", 40, 8),
                    ],
                ),
                ("src/a/b/d.rs", vec![def("parse", 5, 8)]),
                ("src/a/b/e.rs", vec![def("parse", 7, 8)]),
                ("src/a/b/c/deep.rs", vec![def("parse", 2, 8)]),
                ("src/a/bc/y.rs", vec![def("parse", 6, 8)]),
                ("src/a/a.rs", vec![def("parse", 3, 8)]),
                ("src/a/z.rs", vec![def("parse", 12, 8), def("parse", 9, 8)]),
                ("src/q/r.rs", vec![def("parse", 1, 8)]),
                ("lib/x.rs", vec![def("parse", 1, 8)]),
                ("root.rs", vec![def("parse", 4, 8)]),
                ("zzz.rs", vec![def("parse", 1, 8)]),
            ],
        );
        let lookup = |from: &str| {
            fx.definitions(&commit(1), Family::Rust, "parse", from)
                .unwrap()
                .unwrap()
        };

        let hits = lookup("src/a/b/c.rs");
        assert_eq!(
            places(&hits),
            [
                ("src/a/b/c.rs", 30),
                ("src/a/b/d.rs", 5),
                ("src/a/b/e.rs", 7),
                // Three directories shared.
                ("src/a/b/c/deep.rs", 2),
                // Two, by path: `src/a/bc` is not inside `src/a/b`.
                ("src/a/a.rs", 3),
                ("src/a/bc/y.rs", 6),
                ("src/a/z.rs", 9),
                ("src/a/z.rs", 12),
                // One.
                ("src/q/r.rs", 1),
                // None, by path.
                ("lib/x.rs", 1),
                ("root.rs", 4),
                ("zzz.rs", 1),
            ]
        );
        let tiers: Vec<_> = hits.iter().map(|h| h.tier).collect();
        let mut expected = vec![Tier::File, Tier::Dir, Tier::Dir];
        expected.extend([Tier::Other; 9]);
        assert_eq!(tiers, expected);
        assert_eq!(
            hits[0],
            Hit {
                path: "src/a/b/c.rs".to_string(),
                line: 30,
                column: 8,
                end_column: 13,
                kind: "function".to_string(),
                snippet: "fn parse() {".to_string(),
                snippet_column: 4,
                tier: Tier::File,
            }
        );

        // The tiers are relative to the file of the click, which need not be in the commit.
        let hits = lookup("lib/y.rs");
        assert_eq!(hits[0].path, "lib/x.rs");
        assert_eq!(hits[0].tier, Tier::Dir);
        assert!(hits[1..].iter().all(|h| h.tier == Tier::Other));
        assert_eq!(hits[1].path, "root.rs");

        // At the top of the tree the directory is the top.
        let hits = lookup("zzz.rs");
        assert_eq!(
            places(&hits)[..3],
            [("zzz.rs", 1), ("root.rs", 4), ("lib/x.rs", 1)]
        );
        assert_eq!(
            hits.iter().map(|h| h.tier.as_str()).collect::<Vec<_>>()[..3],
            ["file", "dir", "other"]
        );

        // The other names of the file are not found, and nothing is found under a name nobody defines.
        assert!(
            fx.definitions(&commit(1), Family::Rust, "nobody", "zzz.rs")
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn directories_are_compared_literally() {
        let fx = TempDb::new("dirs");
        let f = || vec![def("f", 1, 1)];
        rust_commit(
            &fx,
            &commit(1),
            vec![
                ("a_b/main.rs", f()),
                ("a_b/x.rs", f()),
                ("axb/x.rs", f()),
                ("a%b/x.rs", f()),
                ("a_b/sub/x.rs", f()),
                ("dóm/x.rs", f()),
                ("dóm/y.rs", f()),
                ("dómx/y.rs", f()),
                ("dom/y.rs", f()),
            ],
        );
        let in_tier = |from: &str, tier: Tier| -> Vec<String> {
            let hits = fx
                .definitions(&commit(1), Family::Rust, "f", from)
                .unwrap()
                .unwrap();
            hits.into_iter()
                .filter(|h| h.tier == tier)
                .map(|h| h.path)
                .collect()
        };
        assert_eq!(in_tier("a_b/main.rs", Tier::File), ["a_b/main.rs"]);
        assert_eq!(in_tier("a_b/main.rs", Tier::Dir), ["a_b/x.rs"]);
        assert_eq!(in_tier("dóm/x.rs", Tier::File), ["dóm/x.rs"]);
        assert_eq!(in_tier("dóm/x.rs", Tier::Dir), ["dóm/y.rs"]);
        assert_eq!(in_tier("dóm/x.rs", Tier::Other).len(), 7);
    }

    #[test]
    fn at_most_200_definitions_are_returned_and_the_cut_keeps_the_nearest() {
        let fx = TempDb::new("limits");
        let mut blobs = vec![
            blob(1, Grammar::Rust, vec![def("dup", 1, 4)]),
            blob(2, Grammar::Rust, vec![def("dup", 1, 4)]),
        ];
        let mut entries = vec![
            entry("zzzz/z.rs", 1, Grammar::Rust),
            entry("zzzz/y.rs", 2, Grammar::Rust),
        ];
        // More than a lookup returns, all sorting before the nearest files.
        for i in 0..2100 {
            blobs.push(blob(10 + i, Grammar::Rust, vec![def("dup", 1, 4)]));
            entries.push(entry(&format!("d{i:04}/x.rs"), 10 + i, Grammar::Rust));
        }
        fx.insert_blobs(blobs).unwrap();
        fx.save_commit(&commit(1), &entries).unwrap();
        let hits = fx
            .definitions(&commit(1), Family::Rust, "dup", "zzzz/z.rs")
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 200);
        assert_eq!(
            (hits[0].path.as_str(), hits[0].tier),
            ("zzzz/z.rs", Tier::File)
        );
        assert_eq!(
            (hits[1].path.as_str(), hits[1].tier),
            ("zzzz/y.rs", Tier::Dir)
        );
        assert_eq!(hits[2].path, "d0000/x.rs");
        assert_eq!(hits[199].path, "d0197/x.rs");
    }

    #[test]
    fn the_cut_keeps_the_definitions_that_share_the_most_directories_however_late_they_sort() {
        let fx = TempDb::new("nearest-cut");
        let mut blobs = vec![
            blob(1, Grammar::Rust, vec![def("dup", 1, 4)]),
            blob(2, Grammar::Rust, vec![def("dup", 1, 4)]),
        ];
        let mut entries = vec![
            entry("src/zzz/z.rs", 1, Grammar::Rust),
            entry("src/zeta/other/y.rs", 2, Grammar::Rust),
        ];
        // Far more than a lookup returns, none in the tree of the click, all sorting before the nearest files.
        for i in 0..2100 {
            blobs.push(blob(10 + i, Grammar::Rust, vec![def("dup", 1, 4)]));
            entries.push(entry(&format!("a{i:04}/x.rs"), 10 + i, Grammar::Rust));
        }
        fx.insert_blobs(blobs).unwrap();
        fx.save_commit(&commit(1), &entries).unwrap();
        let hits = fx
            .definitions(&commit(1), Family::Rust, "dup", "src/zeta/deep/main.rs")
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 200);
        assert!(hits.iter().all(|h| h.tier == Tier::Other));
        // Two directories shared, one, and none, which go by path.
        assert_eq!(hits[0].path, "src/zeta/other/y.rs");
        assert_eq!(hits[1].path, "src/zzz/z.rs");
        assert_eq!(hits[2].path, "a0000/x.rs");
        assert_eq!(hits[199].path, "a0197/x.rs");
    }

    #[test]
    fn a_click_deeper_in_the_tree_than_any_repository_still_ranks_by_its_outer_directories() {
        let fx = TempDb::new("deep-click");
        let deep = "d/".repeat(100);
        let near = format!("{deep}other/y.rs");
        let f = || vec![def("f", 1, 1)];
        rust_commit(
            &fx,
            &commit(1),
            vec![("zz/y.rs", f()), (near.as_str(), f()), ("aa/y.rs", f())],
        );
        let hits = fx
            .definitions(&commit(1), Family::Rust, "f", &format!("{deep}main.rs"))
            .unwrap()
            .unwrap();
        assert_eq!(
            places(&hits),
            [(near.as_str(), 1), ("aa/y.rs", 1), ("zz/y.rs", 1)]
        );
    }

    #[test]
    fn lookups_stay_within_the_language_family() {
        let fx = TempDb::new("family");
        let files = [
            ("a.ts", Grammar::TypeScript),
            ("b.tsx", Grammar::Tsx),
            ("c.js", Grammar::JavaScript),
            ("d.py", Grammar::Python),
            ("e.c", Grammar::C),
            ("g.cpp", Grammar::Cpp),
            ("h.rs", Grammar::Rust),
            ("i.go", Grammar::Go),
            ("J.java", Grammar::Java),
        ];
        let mut blobs = Vec::new();
        let mut entries = Vec::new();
        for (i, (path, grammar)) in files.into_iter().enumerate() {
            blobs.push(blob(
                i as u32,
                grammar,
                vec![def("f", 1, 4), call("f", 2, 4)],
            ));
            entries.push(entry(path, i as u32, grammar));
        }
        fx.insert_blobs(blobs).unwrap();
        fx.save_commit(&commit(1), &entries).unwrap();

        let found = |family: Family| -> (Vec<String>, Vec<String>) {
            let mut defs: Vec<_> = fx
                .definitions(&commit(1), family, "f", "a.ts")
                .unwrap()
                .unwrap()
                .into_iter()
                .map(|h| h.path)
                .collect();
            let mut refs: Vec<_> = fx
                .references(&commit(1), family, "f", "a.ts")
                .unwrap()
                .unwrap()
                .0
                .into_iter()
                .map(|(h, _)| h.path)
                .collect();
            defs.sort();
            refs.sort();
            (defs, refs)
        };
        let same = |paths: &[&str]| {
            let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
            (paths.clone(), paths)
        };
        assert_eq!(found(Family::Js), same(&["a.ts", "b.tsx", "c.js"]));
        assert_eq!(found(Family::C), same(&["e.c", "g.cpp"]));
        assert_eq!(found(Family::Python), same(&["d.py"]));
        assert_eq!(found(Family::Rust), same(&["h.rs"]));
        assert_eq!(found(Family::Go), same(&["i.go"]));
        assert_eq!(found(Family::Java), same(&["J.java"]));
    }

    #[test]
    fn constructors_are_symbols_but_not_definitions() {
        let fx = TempDb::new("constructors");
        let class = tag("Foo", "class", true, 1, 7);
        let constructor = tag("Foo", "constructor", true, 3, 5);
        fx.insert_blobs(vec![blob(
            1,
            Grammar::Java,
            vec![class.clone(), constructor.clone(), call("Foo", 9, 13)],
        )])
        .unwrap();
        fx.save_commit(&commit(1), &[entry("Foo.java", 1, Grammar::Java)])
            .unwrap();
        let hits = fx
            .definitions(&commit(1), Family::Java, "Foo", "Bar.java")
            .unwrap()
            .unwrap();
        assert_eq!(places(&hits), [("Foo.java", 1)]);
        assert_eq!(hits[0].kind, "class");
        let stored = fx.blob_symbols(&oid(1), Grammar::Java).unwrap().unwrap();
        assert_eq!(stored.symbols, [class, constructor]);
    }

    #[test]
    fn a_lookup_sees_only_the_blobs_of_its_commit() {
        let fx = TempDb::new("commits");
        let both = || vec![def("f", 1, 4), call("f", 2, 4)];
        fx.insert_blobs(vec![
            blob(1, Grammar::Rust, both()),
            blob(2, Grammar::Rust, both()),
            blob(3, Grammar::Rust, both()),
        ])
        .unwrap();
        fx.save_commit(
            &commit(1),
            &[
                entry("p1.rs", 1, Grammar::Rust),
                entry("p2.rs", 2, Grammar::Rust),
            ],
        )
        .unwrap();
        fx.save_commit(
            &commit(2),
            &[
                entry("p1.rs", 1, Grammar::Rust),
                entry("p3.rs", 3, Grammar::Rust),
            ],
        )
        .unwrap();
        let found = |commit: String| {
            let defs = fx
                .definitions(&commit, Family::Rust, "f", "p1.rs")
                .unwrap()
                .unwrap();
            let refs = fx
                .references(&commit, Family::Rust, "f", "p1.rs")
                .unwrap()
                .unwrap()
                .0;
            (
                places(&defs)
                    .into_iter()
                    .map(|(p, l)| (p.to_string(), l))
                    .collect::<Vec<_>>(),
                refs.iter()
                    .map(|(h, o)| (h.path.clone(), h.line, o.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let (defs, refs) = found(commit(1));
        assert_eq!(defs, [("p1.rs".to_string(), 1), ("p2.rs".to_string(), 1)]);
        assert_eq!(
            refs,
            [
                ("p1.rs".to_string(), 2, oid(1)),
                ("p2.rs".to_string(), 2, oid(2))
            ]
        );
        let (defs, refs) = found(commit(2));
        assert_eq!(defs, [("p1.rs".to_string(), 1), ("p3.rs".to_string(), 1)]);
        assert_eq!(
            refs,
            [
                ("p1.rs".to_string(), 2, oid(1)),
                ("p3.rs".to_string(), 2, oid(3))
            ]
        );
    }

    #[test]
    fn a_blob_at_two_paths_is_found_at_both() {
        let fx = TempDb::new("twice");
        fx.insert_blobs(vec![blob(
            1,
            Grammar::Rust,
            vec![def("f", 1, 4), call("f", 2, 4)],
        )])
        .unwrap();
        fx.save_commit(
            &commit(1),
            &[
                entry("x/a.rs", 1, Grammar::Rust),
                entry("y/a.rs", 1, Grammar::Rust),
            ],
        )
        .unwrap();
        let defs = fx
            .definitions(&commit(1), Family::Rust, "f", "x/a.rs")
            .unwrap()
            .unwrap();
        assert_eq!(places(&defs), [("x/a.rs", 1), ("y/a.rs", 1)]);
        let (refs, truncated) = fx
            .references(&commit(1), Family::Rust, "f", "x/a.rs")
            .unwrap()
            .unwrap();
        assert!(!truncated);
        let found: Vec<_> = refs
            .iter()
            .map(|(h, o)| (h.path.as_str(), o.as_str()))
            .collect();
        let oid1 = oid(1);
        assert_eq!(
            found,
            [("x/a.rs", oid1.as_str()), ("y/a.rs", oid1.as_str())]
        );
    }

    #[test]
    fn names_match_exactly() {
        let fx = TempDb::new("names");
        let names = ["a_b", "axb", "A_B", "a%b", "größe", "日本", "a", "ab"];
        let tags: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| def(name, i as u32 + 1, 1))
            .collect();
        rust_commit(&fx, &commit(1), vec![("dir/日本.rs", tags)]);
        for (i, name) in names.iter().enumerate() {
            let hits = fx
                .definitions(&commit(1), Family::Rust, name, "dir/日本.rs")
                .unwrap()
                .unwrap();
            assert_eq!(places(&hits), [("dir/日本.rs", i as u32 + 1)], "{name}");
        }
        for name in ["", "%", "_", "a_", "a%", "GRÖSSE", "größ", "B", "a_b "] {
            let hits = fx
                .definitions(&commit(1), Family::Rust, name, "dir/日本.rs")
                .unwrap()
                .unwrap();
            assert!(hits.is_empty(), "{name:?}");
        }
    }

    #[test]
    fn references_come_nearest_first_and_carry_their_blob() {
        let fx = TempDb::new("references");
        let ty = tag("name", "type", false, 3, 3);
        let blobs = vec![
            blob(
                1,
                Grammar::Rust,
                vec![def("name", 1, 4), call("name", 5, 9), call("name", 2, 3)],
            ),
            blob(2, Grammar::Rust, vec![call("name", 8, 1)]),
            blob(
                3,
                Grammar::Rust,
                vec![call("name", 4, 1), call("name", 1, 1)],
            ),
            blob(4, Grammar::Rust, vec![call("name", 1, 1), ty]),
            blob(5, Grammar::Go, vec![call("name", 1, 1)]),
        ];
        fx.insert_blobs(blobs).unwrap();
        fx.save_commit(
            &commit(1),
            &[
                entry("x/from.rs", 1, Grammar::Rust),
                entry("x/near.rs", 2, Grammar::Rust),
                entry("b/far.rs", 3, Grammar::Rust),
                entry("a/far.rs", 4, Grammar::Rust),
                entry("x/other.go", 5, Grammar::Go),
            ],
        )
        .unwrap();
        let (refs, truncated) = fx
            .references(&commit(1), Family::Rust, "name", "x/from.rs")
            .unwrap()
            .unwrap();
        assert!(!truncated);
        let found: Vec<_> = refs
            .iter()
            .map(|(h, o)| (h.path.as_str(), h.line, h.tier, h.kind.as_str(), o.clone()))
            .collect();
        assert_eq!(
            found,
            [
                ("x/from.rs", 2, Tier::File, "call", oid(1)),
                ("x/from.rs", 5, Tier::File, "call", oid(1)),
                ("x/near.rs", 8, Tier::Dir, "call", oid(2)),
                ("a/far.rs", 1, Tier::Other, "call", oid(4)),
                ("a/far.rs", 3, Tier::Other, "type", oid(4)),
                ("b/far.rs", 1, Tier::Other, "call", oid(3)),
                ("b/far.rs", 4, Tier::Other, "call", oid(3)),
            ]
        );
        // The caller reads the snippets from the blobs.
        assert!(
            refs.iter()
                .all(|(h, _)| h.snippet.is_empty() && h.snippet_column == 0)
        );
        assert_eq!((refs[0].0.column, refs[0].0.end_column), (3, 7));
        // Definitions are not references.
        assert!(
            refs.iter()
                .all(|(h, _)| (h.path.as_str(), h.line) != ("x/from.rs", 1))
        );
    }

    #[test]
    fn references_stop_at_1000_and_say_when_there_are_more() {
        let fx = TempDb::new("truncation");
        let calls = |n: u32| {
            (1..=n)
                .map(|line| call("many", line, 5))
                .collect::<Vec<_>>()
        };
        fx.insert_blobs(vec![
            blob(1, Grammar::Rust, calls(1000)),
            blob(2, Grammar::Rust, calls(1001)),
        ])
        .unwrap();
        fx.save_commit(&commit(1), &[entry("a.rs", 1, Grammar::Rust)])
            .unwrap();
        fx.save_commit(&commit(2), &[entry("a.rs", 2, Grammar::Rust)])
            .unwrap();
        for (c, truncated) in [(commit(1), false), (commit(2), true)] {
            let (refs, more) = fx
                .references(&c, Family::Rust, "many", "a.rs")
                .unwrap()
                .unwrap();
            assert_eq!(refs.len(), 1000);
            assert_eq!(more, truncated);
            assert_eq!(refs[0].0.line, 1);
            assert_eq!(refs[999].0.line, 1000);
        }
    }

    #[test]
    fn the_nearest_references_survive_the_cut() {
        let fx = TempDb::new("cut");
        let far: Vec<_> = (1..=1500).map(|line| call("hot", line, 1)).collect();
        rust_commit(
            &fx,
            &commit(1),
            vec![
                ("x/from.rs", vec![call("hot", 700, 1), call("hot", 900, 1)]),
                ("x/near.rs", vec![call("hot", 5, 1)]),
                // Sorts before the nearest files.
                ("a/far.rs", far),
            ],
        );
        let (refs, truncated) = fx
            .references(&commit(1), Family::Rust, "hot", "x/from.rs")
            .unwrap()
            .unwrap();
        assert!(truncated);
        assert_eq!(refs.len(), 1000);
        let found: Vec<_> = refs
            .iter()
            .map(|(h, _)| (h.path.as_str(), h.line))
            .collect();
        assert_eq!(
            found[..4],
            [
                ("x/from.rs", 700),
                ("x/from.rs", 900),
                ("x/near.rs", 5),
                ("a/far.rs", 1)
            ]
        );
        assert_eq!(found[999], ("a/far.rs", 997));
    }

    #[test]
    fn saving_and_looking_up_note_that_a_commit_was_used() {
        let fx = TempDb::new("used");
        let now = clock::unix_now() as i64;
        rust_commit(&fx, &commit(1), vec![("a.rs", vec![def("f", 1, 4)])]);
        let used_at = || {
            fx.count(&format!(
                "SELECT used_at FROM commits WHERE oid = '{}'",
                commit(1)
            ))
        };
        assert!(used_at() >= now);

        // A recent note is left as it is.
        fx.side()
            .execute("UPDATE commits SET used_at = ?1", [now - 60])
            .unwrap();
        fx.definitions(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap();
        fx.references(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap();
        assert_eq!(used_at(), now - 60);

        // An old one is refreshed by either lookup.
        fx.side()
            .execute("UPDATE commits SET used_at = 1000", [])
            .unwrap();
        fx.definitions(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap();
        assert!(used_at() >= now);
        fx.side()
            .execute("UPDATE commits SET used_at = 1000", [])
            .unwrap();
        fx.references(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap();
        assert!(used_at() >= now);
    }

    #[test]
    fn lookups_do_not_wait_for_writers() {
        let fx = TempDb::new("busy");
        rust_commit(&fx, &commit(1), vec![("a.rs", vec![def("f", 1, 4)])]);
        let used_at = || {
            fx.count(&format!(
                "SELECT used_at FROM commits WHERE oid = '{}'",
                commit(1)
            ))
        };
        let long_ago = 1000;
        fx.side()
            .execute("UPDATE commits SET used_at = ?1", [long_ago])
            .unwrap();

        // Another server is in the middle of a write: the lookup reads on, and the note of use is skipped.
        let other = fx.side();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        let hits = fx
            .definitions(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(used_at(), long_ago);
        other.execute_batch("ROLLBACK").unwrap();

        // So does this server's own writer being busy.
        let writer = lock(&fx.writer);
        let (sender, receiver) = std::sync::mpsc::channel();
        let db = fx.db.clone();
        let c = commit(1);
        std::thread::spawn(move || {
            let _ = sender.send(db.definitions(&c, Family::Rust, "f", "a.rs"));
        });
        let hits = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the lookup waited for the writer");
        assert_eq!(hits.unwrap().unwrap().len(), 1);
        drop(writer);
        assert_eq!(used_at(), long_ago);

        // Left alone, the lookup notes the use.
        let now = clock::unix_now() as i64;
        fx.definitions(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap();
        assert!(used_at() >= now);
    }

    #[test]
    fn a_build_asking_for_every_blob_and_a_click_do_not_wait_for_each_other() {
        let fx = TempDb::new("bulk-reader");
        rust_commit(&fx, &commit(1), vec![("a.rs", vec![def("f", 1, 4)])]);

        // A click is in the middle of a lookup: the build reads on.
        let click = lock(&fx.reader);
        let (sender, receiver) = std::sync::mpsc::channel();
        let db = fx.db.clone();
        std::thread::spawn(move || {
            let _ = sender.send(db.stored_blobs(&[oid(1), oid(2)]));
        });
        let stored = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the build waited for the lookup of a click")
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[&oid(1)], [(Grammar::Rust, BlobStatus::Parsed)]);
        drop(click);

        // And the build is in the middle of a long read: the click goes through.
        let build = lock(&fx.bulk_reader);
        let (sender, receiver) = std::sync::mpsc::channel();
        let db = fx.db.clone();
        let c = commit(1);
        std::thread::spawn(move || {
            let _ = sender.send(db.definitions(&c, Family::Rust, "f", "a.rs"));
        });
        let hits = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the lookup of a click waited for the build");
        assert_eq!(hits.unwrap().unwrap().len(), 1);
        drop(build);
    }

    #[test]
    fn a_panic_in_a_transaction_rolls_it_back_and_the_next_write_goes_on() {
        let fx = TempDb::new("panic");
        let db = fx.db.clone();
        let crashed = std::thread::spawn(move || {
            let mut conn = lock(&db.writer);
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute("INSERT INTO names (name) VALUES ('half')", [])
                .unwrap();
            panic!("a panic in the middle of a transaction");
        })
        .join();
        assert!(crashed.is_err());
        assert!(fx.writer.is_poisoned());
        assert_eq!(fx.count("SELECT count(*) FROM names"), 0);

        // The lock is poisoned, and the next write takes it all the same.
        rust_commit(&fx, &commit(1), vec![("a.rs", vec![def("f", 1, 4)])]);
        assert_eq!(
            fx.count("SELECT count(*) FROM names WHERE name = 'half'"),
            0
        );
        let hits = fx
            .definitions(&commit(1), Family::Rust, "f", "a.rs")
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn servers_and_jobs_may_store_the_same_blobs_at_once() {
        let fx = TempDb::new("concurrent");
        let other_server = IndexDb::open(&fx.path).unwrap();
        let handles = [
            fx.db.clone(),
            fx.db.clone(),
            other_server.clone(),
            other_server,
        ];
        std::thread::scope(|scope| {
            for (n, db) in handles.iter().enumerate() {
                scope.spawn(move || {
                    for round in 0..15u32 {
                        // Overlapping windows of the same blobs.
                        let start = (round * 10 + n as u32 * 5) % 150;
                        let blobs = (start..start + 50)
                            .map(|i| {
                                blob(
                                    i,
                                    Grammar::Rust,
                                    vec![
                                        def(&format!("f{}", i % 9), i + 1, 4),
                                        call(&format!("g{}", i % 4), i + 1, 9),
                                    ],
                                )
                            })
                            .collect();
                        db.insert_blobs(blobs).unwrap();
                        db.stored_blobs(&[oid(start)]).unwrap();
                        db.commit_complete(&commit(1)).unwrap();
                    }
                });
            }
        });
        let blobs = fx.count("SELECT count(*) FROM blobs");
        assert!(blobs >= 50);
        assert_eq!(fx.count("SELECT count(*) FROM tags"), 2 * blobs);
        assert_eq!(
            fx.count(
                "SELECT count(*) FROM blobs b WHERE (SELECT count(*) FROM tags t WHERE t.blob_id = b.id) <> 2"
            ),
            0
        );
        assert_eq!(fx.count("SELECT count(*) FROM names"), 9 + 4);
    }

    #[test]
    fn what_the_parser_found_comes_back_as_it_was() {
        let fx = TempDb::new("parsed");
        let source = "pub struct Foo;\n\nimpl Foo {\n    pub fn new() -> Foo {\n        Foo\n    }\n}\n\nfn main() {\n    let f = Foo::new();\n    helper(f);\n}\n\nfn helper(_f: Foo) {}\n";
        let parsed = tags::parse(Grammar::Rust, source.as_bytes(), None).unwrap();
        fx.insert_blobs(vec![NewBlob {
            oid: oid(1),
            grammar: Grammar::Rust,
            status: BlobStatus::Parsed,
            size: source.len() as u64,
            tags: parsed.clone(),
        }])
        .unwrap();
        fx.save_commit(&commit(1), &[entry("src/lib.rs", 1, Grammar::Rust)])
            .unwrap();

        let mut symbols: Vec<_> = parsed
            .iter()
            .filter(|t| t.is_def || t.kind == "implementation")
            .cloned()
            .collect();
        symbols.sort_by_key(|t| (t.scope_start, t.column));
        assert!(symbols.iter().any(|t| t.kind == "implementation"));
        let stored = fx.blob_symbols(&oid(1), Grammar::Rust).unwrap().unwrap();
        assert_eq!(stored.symbols, symbols);

        let foo = parsed.iter().find(|t| t.is_def && t.name == "Foo").unwrap();
        let hits = fx
            .definitions(&commit(1), Family::Rust, "Foo", "src/lib.rs")
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(
            (hit.line, hit.column, hit.end_column, hit.kind.as_str()),
            (foo.line, foo.column, foo.end_column, foo.kind.as_str())
        );
        assert_eq!(
            (hit.snippet.as_str(), hit.snippet_column),
            ("pub struct Foo;", foo.snippet_column)
        );

        // Every reference to the name, none of them on its definition.
        let expected: Vec<_> = parsed
            .iter()
            .filter(|t| !t.is_def && t.name == "Foo")
            .map(|t| (t.line, t.column, t.end_column, t.kind.clone()))
            .collect();
        assert!(expected.len() >= 3);
        assert!(
            expected
                .iter()
                .all(|r| (r.0, r.1) != (foo.line, foo.column))
        );
        let (refs, _) = fx
            .references(&commit(1), Family::Rust, "Foo", "src/lib.rs")
            .unwrap()
            .unwrap();
        let found: Vec<_> = refs
            .iter()
            .map(|(h, _)| (h.line, h.column, h.end_column, h.kind.clone()))
            .collect();
        assert_eq!(found, expected);
    }
}
