//! Local state that survives restarts, in `$XDG_CACHE_HOME/tideline/state.sqlite`.
//!
//! One connection per server, behind a mutex; every call is tiny and runs on the blocking
//! pool. Several servers may share the cache directory, hence WAL and a busy timeout.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, params};

/// Schema migrations; `PRAGMA user_version` is the number already applied.
const MIGRATIONS: &[&str] = &["CREATE TABLE seen_head (
        host TEXT NOT NULL,
        owner TEXT NOT NULL,
        repo TEXT NOT NULL,
        number INTEGER NOT NULL,
        head_sha TEXT NOT NULL,
        recorded_at INTEGER NOT NULL,
        PRIMARY KEY (host, owner, repo, number)
    )"];

/// Identifies one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrKey {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

#[derive(Clone)]
pub struct StateDb {
    conn: Arc<Mutex<Connection>>,
}

impl StateDb {
    pub fn open(path: &Path) -> Result<StateDb, String> {
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
        let mut conn = Connection::open(path).map_err(|e| fail(&e))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| fail(&e))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
            .map_err(|e| fail(&e))?;
        migrate(&mut conn).map_err(|e| fail(&e))?;
        Ok(StateDb {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || f(&conn.lock().expect("state db poisoned")))
            .await
            .map_err(|e| format!("state database task failed: {e}"))?
            .map_err(|e| format!("state database: {e}"))
    }

    /// Remembers `head` as the last head the user finished viewing in this tool.
    pub async fn set_seen_head(&self, pr: &PrKey, head: &str, at: u64) -> Result<(), String> {
        let pr = pr.clone();
        let head = head.to_string();
        self.call(move |c| {
            c.execute(
                "INSERT INTO seen_head (host, owner, repo, number, head_sha, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (host, owner, repo, number)
                 DO UPDATE SET head_sha = excluded.head_sha, recorded_at = excluded.recorded_at",
                params![
                    pr.host,
                    pr.owner,
                    pr.repo,
                    pr.number as i64,
                    head,
                    at as i64
                ],
            )
            .map(|_| ())
        })
        .await
    }
}

/// Applies pending migrations. `BEGIN IMMEDIATE` serializes concurrent servers, and the
/// version is re-read inside the transaction so each migration runs exactly once.
fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let applied: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    // A newer gh-tideline may have migrated further; leave its schema alone.
    if applied >= MIGRATIONS.len() as i64 {
        return Ok(());
    }
    for sql in &MIGRATIONS[applied as usize..] {
        tx.execute_batch(sql)?;
    }
    tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> PrKey {
        PrKey {
            host: "github.com".into(),
            owner: "o".into(),
            repo: "r".into(),
            number: 7,
        }
    }

    fn seen(path: &Path) -> Vec<(String, i64)> {
        let c = Connection::open(path).unwrap();
        let mut stmt = c
            .prepare("SELECT head_sha, recorded_at FROM seen_head")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[tokio::test]
    async fn seen_head_upserts_and_file_is_private() {
        let dir = std::env::temp_dir().join(format!("tideline-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.sqlite");
        let _ = std::fs::remove_file(&path);

        let db = StateDb::open(&path).unwrap();
        db.set_seen_head(&key(), "a".repeat(40).as_str(), 10)
            .await
            .unwrap();
        db.set_seen_head(&key(), "b".repeat(40).as_str(), 20)
            .await
            .unwrap();
        assert_eq!(seen(&path), vec![("b".repeat(40), 20)]);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Reopening (as a second server would) keeps the data and re-runs no migration.
        drop(db);
        StateDb::open(&path).unwrap();
        assert_eq!(seen(&path), vec![("b".repeat(40), 20)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
