use rusqlite::Connection;
use std::{error::Error, path::Path, time::Duration};

/// Open the SQLite database. Tables of the Go server's v1 schema are never
/// altered; contract v2 only adds new tables (ledgers, attachments).
pub fn open(path: &str) -> Result<Connection, Box<dyn Error + Send + Sync>> {
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        // Match Go's MkdirAll(..., 0750): the database directory must not be
        // accessible to other local users under the usual 022 umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o750);
        }
        builder.create(parent)?;
    }
    let db = Connection::open(path)?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.busy_timeout(Duration::from_secs(5))?;
    migrate(&db)?;
    Ok(db)
}

pub fn migrate(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE COLLATE NOCASE,
  password_hash TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS refresh_tokens (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  token_hash TEXT NOT NULL UNIQUE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  revoked_at TEXT
);
CREATE INDEX IF NOT EXISTS refresh_tokens_user_idx ON refresh_tokens(user_id);
CREATE TABLE IF NOT EXISTS records (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  entity TEXT NOT NULL,
  record_id TEXT NOT NULL,
  payload TEXT,
  updated_at TEXT NOT NULL,
  deleted INTEGER NOT NULL DEFAULT 0,
  version INTEGER NOT NULL,
  PRIMARY KEY (user_id, entity, record_id)
);
CREATE TABLE IF NOT EXISTS changes (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  entity TEXT NOT NULL,
  record_id TEXT NOT NULL,
  payload TEXT,
  updated_at TEXT NOT NULL,
  deleted INTEGER NOT NULL,
  version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS changes_user_sequence_idx ON changes(user_id, sequence);
CREATE TABLE IF NOT EXISTS ledgers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  owner_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ledger_members (
  ledger_id TEXT NOT NULL REFERENCES ledgers(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('owner','member')),
  joined_at TEXT NOT NULL,
  PRIMARY KEY (ledger_id, user_id)
);
CREATE INDEX IF NOT EXISTS ledger_members_user_idx ON ledger_members(user_id);
CREATE TABLE IF NOT EXISTS ledger_invites (
  code_hash TEXT PRIMARY KEY,
  ledger_id TEXT NOT NULL REFERENCES ledgers(id) ON DELETE CASCADE,
  created_by TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  used_at TEXT,
  used_by TEXT REFERENCES users(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS ledger_invites_ledger_idx ON ledger_invites(ledger_id);
CREATE TABLE IF NOT EXISTS ledger_records (
  ledger_id TEXT NOT NULL REFERENCES ledgers(id) ON DELETE CASCADE,
  entity TEXT NOT NULL,
  record_id TEXT NOT NULL,
  payload TEXT,
  updated_at TEXT NOT NULL,
  deleted INTEGER NOT NULL DEFAULT 0,
  version INTEGER NOT NULL,
  PRIMARY KEY (ledger_id, entity, record_id)
);
CREATE TABLE IF NOT EXISTS ledger_changes (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  ledger_id TEXT NOT NULL REFERENCES ledgers(id) ON DELETE CASCADE,
  entity TEXT NOT NULL,
  record_id TEXT NOT NULL,
  payload TEXT,
  updated_at TEXT NOT NULL,
  deleted INTEGER NOT NULL,
  version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ledger_changes_ledger_sequence_idx ON ledger_changes(ledger_id, sequence);
CREATE TABLE IF NOT EXISTS attachment_blobs (
  space TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  content_type TEXT NOT NULL,
  size INTEGER NOT NULL,
  uploaded_at TEXT NOT NULL,
  referenced INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (space, sha256)
);",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_repeatable_and_enforces_existing_constraints() {
        let db = open(":memory:").unwrap();
        migrate(&db).unwrap();
        db.execute(
            "INSERT INTO users VALUES ('u','Test@example.com','hash','now')",
            [],
        )
        .unwrap();
        assert!(
            db.execute(
                "INSERT INTO users VALUES ('v','test@example.com','hash','now')",
                []
            )
            .is_err()
        );
        assert!(
            db.execute(
                "INSERT INTO records VALUES ('missing','category','c',NULL,'now',1,1)",
                []
            )
            .is_err()
        );
        db.execute("INSERT INTO refresh_tokens(token_hash,user_id,expires_at,created_at) VALUES ('hash','u','then','now')", []).unwrap();
        db.execute("DELETE FROM users WHERE id='u'", []).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM refresh_tokens", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn opens_populated_go_schema_without_changing_rows_or_sequence() {
        let directory = std::env::temp_dir().join(format!(
            "balance-go-db-{}",
            crate::auth::identifier().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("existing.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            // Use the original Go schema, so a divergence is caught even when
            // Rust's migration is internally consistent.
            db.execute_batch(include_str!("../tests/fixtures/go_schema_v1.sql"))
                .unwrap();
            db.execute_batch(
                "INSERT INTO users VALUES ('go-user','go@example.com','existing hash','2026-01-01T00:00:00.123456789Z');
                 INSERT INTO refresh_tokens(id,token_hash,user_id,expires_at,created_at) VALUES (7,'existing-token-hash','go-user','2099-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO records VALUES ('go-user','transaction','record', '{\"amount\":12.5}', '2026-01-01T00:00:00.123456789Z',0,41);
                 INSERT INTO changes VALUES (41,'go-user','transaction','record','{\"amount\":12.5}','2026-01-01T00:00:00.123456789Z',0,41);"
            ).unwrap();
        }
        {
            let db = open(path.to_str().unwrap()).unwrap();
            let user: (String, String) = db
                .query_row(
                    "SELECT password_hash,created_at FROM users WHERE id='go-user'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                user,
                (
                    "existing hash".into(),
                    "2026-01-01T00:00:00.123456789Z".into()
                )
            );
            assert_eq!(
                db.query_row(
                    "SELECT id FROM refresh_tokens WHERE revoked_at IS NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                7
            );
            assert_eq!(
                db.query_row("SELECT payload FROM records", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "{\"amount\":12.5}"
            );
            // Contract v2 tables are added next to the untouched v1 tables.
            for table in [
                "ledgers",
                "ledger_members",
                "ledger_invites",
                "ledger_records",
                "ledger_changes",
                "attachment_blobs",
            ] {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
                        [table],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    1,
                    "{table}"
                );
            }
            db.execute("INSERT INTO changes(user_id,entity,record_id,payload,updated_at,deleted,version) VALUES ('go-user','transaction','next',NULL,'now',1,42)", []).unwrap();
            assert_eq!(db.last_insert_rowid(), 42);
            assert_eq!(
                db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                db.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "wal"
            );
            assert_eq!(
                db.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                5000
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
