//! Attachment blobs (contract v2, section 8).
//!
//! Bytes live on disk under the attachments directory, one subdirectory per
//! data space; their metadata lives in `attachment_blobs`. A blob is garbage
//! when no live `attachment` record of its space references its SHA-256.
//! Blobs that were uploaded but not referenced yet are kept for a grace period,
//! because clients upload the bytes before syncing the record.

use crate::sync::Space;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const MAX_BYTES: usize = 10 * 1024 * 1024;
pub const TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/heic", "application/pdf"];
/// How long an uploaded, never referenced blob survives garbage collection.
pub const GRACE: TimeDelta = TimeDelta::hours(24);

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn space_dir(&self, space: &Space) -> PathBuf {
        let (kind, id) = match space {
            Space::User(id) => ("users", id.as_str()),
            Space::Ledger(id) => ("ledgers", id.as_str()),
        };
        // Server-generated ids are hex/UUIDs; anything else is hashed so a
        // database value can never become a path component such as "..".
        let safe = !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        let name = if safe {
            id.to_owned()
        } else {
            crate::auth::token_hash(id)
        };
        self.dir.join(kind).join(name)
    }

    fn path(&self, space: &Space, sha: &str) -> PathBuf {
        self.space_dir(space).join(&sha[..2]).join(sha)
    }

    /// Write bytes to a temporary file of the space, ready to be renamed.
    fn stage(&self, space: &Space, bytes: &[u8]) -> io::Result<PathBuf> {
        let dir = self.space_dir(space);
        create_dir(&dir)?;
        let name = crate::auth::identifier().map_err(io::Error::other)?;
        let path = dir.join(format!(".upload-{name}"));
        let mut file = fs::File::create(&path)?;
        let written = file.write_all(bytes).and_then(|_| file.sync_all());
        if let Err(e) = written {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        Ok(path)
    }

    fn install(&self, staged: &Path, space: &Space, sha: &str) -> io::Result<()> {
        let path = self.path(space, sha);
        if let Some(parent) = path.parent() {
            create_dir(parent)?;
        }
        fs::rename(staged, path)
    }

    pub fn read(&self, space: &Space, sha: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path(space, sha))
    }

    /// Remove blob files whose rows were already deleted.
    pub fn remove(&self, space: &Space, shas: &[String]) {
        for sha in shas {
            if valid_sha(sha) {
                match fs::remove_file(self.path(space, sha)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => eprintln!("attachment cleanup failed: {}", e.kind()),
                }
            }
        }
    }

    /// Remove every blob of a deleted space.
    pub fn remove_space(&self, space: &Space) {
        match fs::remove_dir_all(self.space_dir(space)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("attachment directory cleanup failed: {}", e.kind()),
        }
    }
}

fn create_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o750);
    }
    builder.create(path)
}

/// SHA-256 in the canonical form of the contract: 64 lowercase hex digits.
pub fn valid_sha(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Accepted media type of a `Content-Type` header, ignoring parameters.
pub fn media_type(header: &str) -> Option<&'static str> {
    let essence = header.split(';').next()?.trim().to_ascii_lowercase();
    TYPES.into_iter().find(|t| *t == essence)
}

fn stamp(t: DateTime<Utc>) -> String {
    // Fixed width, so stored stamps compare correctly as text.
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn live_shas(space: &Space) -> String {
    let (records, _, key) = space.tables();
    format!(
        "SELECT sha FROM (SELECT lower(CASE WHEN json_valid(payload) THEN json_extract(payload,'$.sha256') END) AS sha FROM {records} WHERE {key}=?2 AND entity='attachment' AND deleted=0) WHERE sha IS NOT NULL"
    )
}

/// Delete rows of blobs no live attachment record references and return
/// their hashes. Blobs never referenced survive for [`GRACE`] after upload.
pub fn collect_garbage(
    db: &Connection,
    space: &Space,
    now: DateTime<Utc>,
) -> rusqlite::Result<Vec<String>> {
    let live = live_shas(space);
    let key = space.key();
    db.execute(
        &format!(
            "UPDATE attachment_blobs SET referenced=1 WHERE space=?1 AND referenced=0 AND sha256 IN ({live})"
        ),
        params![key, space.id()],
    )?;
    let condition =
        format!("space=?1 AND (referenced=1 OR uploaded_at<?3) AND sha256 NOT IN ({live})");
    let garbage = db
        .prepare(&format!(
            "SELECT sha256 FROM attachment_blobs WHERE {condition}"
        ))?
        .query_map(params![key, space.id(), stamp(now - GRACE)], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    db.execute(
        &format!("DELETE FROM attachment_blobs WHERE {condition}"),
        params![key, space.id(), stamp(now - GRACE)],
    )?;
    Ok(garbage)
}

#[derive(Debug, PartialEq, Eq)]
pub enum PutError {
    HashMismatch,
    UnsupportedMediaType,
    PayloadTooLarge,
    Internal,
}

/// Store bytes in `space`. Returns `true` when the blob was created and
/// `false` for an idempotent repeat. The database lock is taken only to
/// install the already written file.
pub fn put(
    db: &std::sync::Mutex<Connection>,
    store: &Store,
    space: &Space,
    sha: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<bool, PutError> {
    if !valid_sha(sha) {
        return Err(PutError::HashMismatch);
    }
    let Some(media) = media_type(content_type) else {
        return Err(PutError::UnsupportedMediaType);
    };
    if bytes.len() > MAX_BYTES {
        return Err(PutError::PayloadTooLarge);
    }
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != sha {
        return Err(PutError::HashMismatch);
    }
    let staged = store.stage(space, bytes).map_err(|_| PutError::Internal)?;
    let result = install(db, store, space, sha, media, bytes.len(), &staged);
    let _ = fs::remove_file(&staged);
    result
}

fn install(
    db: &std::sync::Mutex<Connection>,
    store: &Store,
    space: &Space,
    sha: &str,
    media: &str,
    size: usize,
    staged: &Path,
) -> Result<bool, PutError> {
    let internal = |_| PutError::Internal;
    let mut db = db.lock().unwrap_or_else(|e| e.into_inner());
    let tx = db.transaction().map_err(internal)?;
    let now = Utc::now();
    let key = space.key();
    let exists = tx
        .query_row(
            "SELECT 1 FROM attachment_blobs WHERE space=? AND sha256=?",
            params![key, sha],
            |_| Ok(()),
        )
        .optional()
        .map_err(internal)?
        .is_some();
    let path = store.path(space, sha);
    let file_missing = !path.is_file();
    if exists {
        // A repeated upload restarts the grace period: another device may
        // be about to sync a new record that references these bytes.
        tx.execute(
            "UPDATE attachment_blobs SET uploaded_at=?, referenced=0 WHERE space=? AND sha256=?",
            params![stamp(now), key, sha],
        )
        .map_err(internal)?;
    } else {
        tx.execute(
            "INSERT INTO attachment_blobs(space,sha256,content_type,size,uploaded_at,referenced) VALUES(?,?,?,?,?,0)",
            params![key, sha, media, size as i64, stamp(now)],
        )
        .map_err(internal)?;
    }
    let garbage = collect_garbage(&tx, space, now).map_err(internal)?;
    if file_missing {
        store
            .install(staged, space, sha)
            .map_err(|_| PutError::Internal)?;
    }
    if tx.commit().is_err() {
        if file_missing {
            let _ = fs::remove_file(&path);
        }
        return Err(PutError::Internal);
    }
    store.remove(space, &garbage);
    Ok(!exists)
}

/// Bytes and media type of a blob stored in `space`.
pub fn get(
    db: &std::sync::Mutex<Connection>,
    store: &Store,
    space: &Space,
    sha: &str,
) -> Result<Option<(Vec<u8>, String)>, ()> {
    if !valid_sha(sha) {
        return Ok(None);
    }
    let db = db.lock().unwrap_or_else(|e| e.into_inner());
    let media: Option<String> = db
        .query_row(
            "SELECT content_type FROM attachment_blobs WHERE space=? AND sha256=?",
            params![space.key(), sha],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ())?;
    let Some(media) = media else {
        return Ok(None);
    };
    // Reading under the lock keeps garbage collection from removing the
    // file between the metadata check and the read.
    match store.read(space, sha) {
        Ok(bytes) => Ok(Some((bytes, media))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_media_types_are_strict() {
        assert!(valid_sha(&"a".repeat(64)));
        assert!(!valid_sha(&"A".repeat(64)));
        assert!(!valid_sha(&"a".repeat(63)));
        assert!(!valid_sha(&format!("{}g", "a".repeat(63))));
        assert_eq!(media_type("IMAGE/JPEG; q=1"), Some("image/jpeg"));
        assert_eq!(media_type("application/pdf"), Some("application/pdf"));
        assert_eq!(media_type("image/gif"), None);
        assert_eq!(media_type(""), None);
    }

    #[test]
    fn unsafe_space_ids_never_become_path_components() {
        let store = Store::new("/base");
        let dir = store.space_dir(&Space::Ledger("../../etc".into()));
        assert!(dir.starts_with("/base/ledgers"));
        assert!(!dir.to_string_lossy().contains(".."));
        assert_eq!(
            store.space_dir(&Space::User("abc".into())),
            Path::new("/base/users/abc")
        );
    }
}
