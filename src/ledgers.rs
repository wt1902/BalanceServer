//! Shared ledgers (contract v2, section 7).
//!
//! A ledger is a data space with its own records, change stream and
//! attachments. Membership is checked for every request that selects it.

use crate::sync::Space;
use chrono::{SecondsFormat, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub type LedgerError = (u16, &'static str, &'static str);

pub const INVITE_TTL: TimeDelta = TimeDelta::days(7);
pub const CODE_LENGTH: usize = 10;
/// 32 symbols without look-alikes (no I, O, 0, 1): five random bits each.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MAX_NAME: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Member,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Member => "member",
        }
    }
}

fn internal<E>(_: E) -> LedgerError {
    (
        500,
        "internal_error",
        "Could not process the ledger request",
    )
}
fn not_found() -> LedgerError {
    (404, "not_found", "Ledger not found")
}
fn forbidden() -> LedgerError {
    (403, "forbidden", "Not allowed for this ledger")
}
fn invalid_code() -> LedgerError {
    (400, "invalid_code", "Invite code is invalid or expired")
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Role of `user` in `ledger`: 404 for an unknown ledger, 403 for a non-member.
pub fn role(db: &Connection, ledger: &str, user: &str) -> Result<Role, LedgerError> {
    let exists = db
        .query_row("SELECT 1 FROM ledgers WHERE id=?", [ledger], |_| Ok(()))
        .optional()
        .map_err(internal)?
        .is_some();
    if !exists {
        return Err(not_found());
    }
    let role: Option<String> = db
        .query_row(
            "SELECT role FROM ledger_members WHERE ledger_id=? AND user_id=?",
            params![ledger, user],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    match role.as_deref() {
        Some("owner") => Ok(Role::Owner),
        Some(_) => Ok(Role::Member),
        None => Err(forbidden()),
    }
}

/// Data space selected by the `X-Balance-Ledger` header value.
pub fn space(db: &Connection, header: &str, user: &str) -> Result<Space, LedgerError> {
    let ledger = header.trim();
    if ledger.is_empty() {
        return Ok(Space::User(user.to_owned()));
    }
    role(db, ledger, user)?;
    Ok(Space::Ledger(ledger.to_owned()))
}

fn describe(db: &Connection, ledger: &str, role: Role) -> Result<Value, LedgerError> {
    let name: String = db
        .query_row("SELECT name FROM ledgers WHERE id=?", [ledger], |r| {
            r.get(0)
        })
        .map_err(internal)?;
    let mut stmt = db
        .prepare(
            "SELECT m.user_id,u.email,m.role FROM ledger_members m JOIN users u ON u.id=m.user_id
             WHERE m.ledger_id=? ORDER BY m.role='owner' DESC, m.joined_at, m.user_id",
        )
        .map_err(internal)?;
    let members = stmt
        .query_map([ledger], |r| {
            Ok(json!({"userId": r.get::<_, String>(0)?, "email": r.get::<_, String>(1)?, "role": r.get::<_, String>(2)?}))
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<Value>>>()
        .map_err(internal)?;
    Ok(json!({"id": ledger, "name": name, "role": role.as_str(), "members": members}))
}

/// Shared ledgers of `user`; the personal space is not listed.
pub fn list(db: &Connection, user: &str) -> Result<Value, LedgerError> {
    let mut stmt = db
        .prepare(
            "SELECT l.id,m.role FROM ledger_members m JOIN ledgers l ON l.id=m.ledger_id
             WHERE m.user_id=? ORDER BY l.created_at, l.id",
        )
        .map_err(internal)?;
    let rows = stmt
        .query_map([user], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)? == "owner"))
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)?;
    let ledgers = rows
        .into_iter()
        .map(|(id, owner)| describe(db, &id, if owner { Role::Owner } else { Role::Member }))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "ledgers": ledgers }))
}

/// Ledger name from a `{"name": …}` body: trimmed, 1–100 characters.
pub fn parse_name(body: &[u8]) -> Result<String, LedgerError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| (400, "invalid_json", "Request body is not valid JSON"))?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err((
            400,
            "invalid_name",
            "Ledger name must contain 1 to 100 characters",
        ));
    }
    Ok(name.to_owned())
}

pub fn create(db: &mut Connection, user: &str, name: &str) -> Result<Value, LedgerError> {
    let id = uuid_v4().map_err(internal)?;
    let tx = db.transaction().map_err(internal)?;
    let created = now();
    tx.execute(
        "INSERT INTO ledgers(id,name,owner_id,created_at) VALUES(?,?,?,?)",
        params![id, name, user, created],
    )
    .map_err(internal)?;
    tx.execute(
        "INSERT INTO ledger_members(ledger_id,user_id,role,joined_at) VALUES(?,?,'owner',?)",
        params![id, user, created],
    )
    .map_err(internal)?;
    let ledger = describe(&tx, &id, Role::Owner)?;
    tx.commit().map_err(internal)?;
    Ok(ledger)
}

pub fn rename(db: &Connection, ledger: &str, user: &str, name: &str) -> Result<Value, LedgerError> {
    if role(db, ledger, user)? != Role::Owner {
        return Err(forbidden());
    }
    db.execute(
        "UPDATE ledgers SET name=? WHERE id=?",
        params![name, ledger],
    )
    .map_err(internal)?;
    describe(db, ledger, Role::Owner)
}

/// Delete a ledger with its records, changes, invites and blob rows. The
/// caller removes the blob directory afterwards.
pub fn delete(db: &mut Connection, ledger: &str, user: &str) -> Result<(), LedgerError> {
    if role(db, ledger, user)? != Role::Owner {
        return Err(forbidden());
    }
    let tx = db.transaction().map_err(internal)?;
    tx.execute(
        "DELETE FROM attachment_blobs WHERE space=?",
        [Space::Ledger(ledger.to_owned()).key()],
    )
    .map_err(internal)?;
    // Cascades to members, invites, records and changes.
    tx.execute("DELETE FROM ledgers WHERE id=?", [ledger])
        .map_err(internal)?;
    tx.commit().map_err(internal)
}

pub fn invite(db: &Connection, ledger: &str, user: &str) -> Result<Value, LedgerError> {
    if role(db, ledger, user)? != Role::Owner {
        return Err(forbidden());
    }
    let code = random_code().map_err(internal)?;
    let created = Utc::now();
    let expires = created + INVITE_TTL;
    db.execute(
        "INSERT INTO ledger_invites(code_hash,ledger_id,created_by,expires_at,created_at) VALUES(?,?,?,?,?)",
        params![
            crate::auth::token_hash(&code),
            ledger,
            user,
            expires.to_rfc3339_opts(SecondsFormat::Nanos, true),
            created.to_rfc3339_opts(SecondsFormat::Nanos, true)
        ],
    )
    .map_err(internal)?;
    Ok(json!({
        "code": code,
        "expiresAt": expires.to_rfc3339_opts(SecondsFormat::Secs, true)
    }))
}

/// Canonical invite code: case-insensitive, spaces and dashes ignored.
pub fn normalize_code(code: &str) -> Option<String> {
    let code: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LENGTH && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

pub fn join(db: &mut Connection, user: &str, body: &[u8]) -> Result<Value, LedgerError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| (400, "invalid_json", "Request body is not valid JSON"))?;
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .and_then(normalize_code)
        .ok_or_else(invalid_code)?;
    let tx = db.transaction().map_err(internal)?;
    let invite: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT ledger_id,expires_at,used_at FROM ledger_invites WHERE code_hash=?",
            [crate::auth::token_hash(&code)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((ledger, expires, used)) = invite else {
        return Err(invalid_code());
    };
    if used.is_some() || expires <= now() {
        return Err(invalid_code());
    }
    // Joining a ledger one already belongs to does not consume the code.
    if let Ok(existing) = role(&tx, &ledger, user) {
        return describe(&tx, &ledger, existing);
    }
    let joined = now();
    tx.execute(
        "UPDATE ledger_invites SET used_at=?, used_by=? WHERE code_hash=?",
        params![joined, user, crate::auth::token_hash(&code)],
    )
    .map_err(internal)?;
    tx.execute(
        "INSERT INTO ledger_members(ledger_id,user_id,role,joined_at) VALUES(?,?,'member',?)",
        params![ledger, user, joined],
    )
    .map_err(internal)?;
    let result = describe(&tx, &ledger, Role::Member)?;
    tx.commit().map_err(internal)?;
    Ok(result)
}

/// The owner removes a member; a member removes only itself.
pub fn remove_member(
    db: &Connection,
    ledger: &str,
    user: &str,
    target: &str,
) -> Result<(), LedgerError> {
    let caller = role(db, ledger, user)?;
    if caller == Role::Member && target != user {
        return Err(forbidden());
    }
    let target_role = match role(db, ledger, target) {
        Ok(r) => r,
        Err((403, _, _)) => return Err((404, "not_found", "Member not found")),
        Err(e) => return Err(e),
    };
    if target_role == Role::Owner {
        // Leaving is impossible while the ledger has an owner; the owner
        // deletes the ledger instead.
        return Err((
            409,
            "owner_cannot_leave",
            "The owner cannot leave the ledger; delete it instead",
        ));
    }
    db.execute(
        "DELETE FROM ledger_members WHERE ledger_id=? AND user_id=?",
        params![ledger, target],
    )
    .map_err(internal)?;
    Ok(())
}

fn random_code() -> Result<String, String> {
    let mut bytes = [0u8; CODE_LENGTH];
    getrandom(&mut bytes)?;
    Ok(bytes
        .iter()
        .map(|b| CODE_ALPHABET[usize::from(b & 31)] as char)
        .collect())
}

fn uuid_v4() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|v| format!("{v:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn getrandom(bytes: &mut [u8]) -> Result<(), String> {
    use rand::RngCore;
    rand::rngs::OsRng
        .try_fill_bytes(bytes)
        .map_err(|_| "random source unavailable".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_ten_unambiguous_symbols_and_normalize() {
        let code = random_code().unwrap();
        assert_eq!(code.len(), CODE_LENGTH);
        assert_eq!(normalize_code(&code), Some(code.clone()));
        let lower = format!(
            " {}-{} ",
            code[..5].to_lowercase(),
            code[5..].to_lowercase()
        );
        assert_eq!(normalize_code(&lower), Some(code));
        assert_eq!(normalize_code("ABCDEFGHI0"), None);
        assert_eq!(normalize_code("ABCDEFGHJ"), None);
        let id = uuid_v4().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
    }
}
