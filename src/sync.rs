use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
#[cfg(test)]
use serde_json::Value;
use serde_json::{json, value::RawValue};
use std::fmt;

pub type SyncError = (u16, &'static str, String);

#[derive(Default)]
struct Request {
    cursor: i64,
    changes: Vec<Change>,
    pull: Option<bool>,
}

#[derive(Default)]
struct Change {
    entity: String,
    id: String,
    deleted: bool,
    updated_at: Option<DateTime<Utc>>,
    payload: Option<Vec<u8>>,
}

// Go's decoder accepts null for ordinary scalar fields, matches keys ignoring
// case, and lets the last duplicate field win. Keep those wire semantics.
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct RequestVisitor;
        impl<'de> Visitor<'de> for RequestVisitor {
            type Value = Request;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a sync request")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Request, E> {
                Ok(Request::default())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<Request, M::Error> {
                let mut r = Request::default();
                while let Some(key) = m.next_key::<String>()? {
                    match fold_key(&key).as_str() {
                        "cursor" => {
                            if let Some(v) = m.next_value::<Option<i64>>()? {
                                r.cursor = v;
                            }
                        }
                        "deviceid" => {
                            let _: Option<String> = m.next_value()?;
                        }
                        "changes" => m.next_value_seed(ChangesSeed(&mut r.changes))?,
                        "pull" => r.pull = m.next_value()?,
                        _ => return Err(de::Error::custom("unknown field")),
                    }
                }
                Ok(r)
            }
        }
        d.deserialize_any(RequestVisitor)
    }
}

// encoding/json reuses existing slice elements when a duplicate changes field
// appears. Missing fields in those elements keep their preceding values.
struct ChangesSeed<'a>(&'a mut Vec<Change>);
impl<'de> DeserializeSeed<'de> for ChangesSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        struct ChangesVisitor<'a>(&'a mut Vec<Change>);
        impl<'de> Visitor<'de> for ChangesVisitor<'_> {
            type Value = ();
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an array of changes")
            }
            fn visit_unit<E: de::Error>(self) -> Result<(), E> {
                self.0.clear();
                Ok(())
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
                let mut index = 0;
                loop {
                    if index == self.0.len() {
                        self.0.push(Change::default());
                    }
                    if seq
                        .next_element_seed(ChangeSeed(&mut self.0[index]))?
                        .is_none()
                    {
                        break;
                    }
                    index += 1;
                }
                self.0.truncate(index);
                Ok(())
            }
        }
        d.deserialize_any(ChangesVisitor(self.0))
    }
}

struct ChangeSeed<'a>(&'a mut Change);
impl<'de> DeserializeSeed<'de> for ChangeSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        struct ChangeVisitor<'a>(&'a mut Change);
        impl<'de> Visitor<'de> for ChangeVisitor<'_> {
            type Value = ();
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a sync change")
            }
            fn visit_unit<E: de::Error>(self) -> Result<(), E> {
                Ok(())
            }
            fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<(), M::Error> {
                let c = self.0;
                while let Some(key) = m.next_key::<String>()? {
                    match fold_key(&key).as_str() {
                        "entity" => {
                            if let Some(v) = m.next_value::<Option<String>>()? {
                                c.entity = v;
                            }
                        }
                        "id" => {
                            if let Some(v) = m.next_value::<Option<String>>()? {
                                c.id = v;
                            }
                        }
                        "deleted" => {
                            if let Some(v) = m.next_value::<Option<bool>>()? {
                                c.deleted = v;
                            }
                        }
                        "updatedat" => {
                            // time.Time.UnmarshalJSON reads the quoted bytes directly;
                            // JSON escapes inside a timestamp are not unescaped by Go.
                            let raw = m.next_value::<Box<RawValue>>()?;
                            if raw.get() != "null" {
                                let value = raw
                                    .get()
                                    .strip_prefix('"')
                                    .and_then(|v| v.strip_suffix('"'))
                                    .ok_or_else(|| de::Error::custom("invalid time"))?;
                                c.updated_at = Some(parse_time(value).map_err(de::Error::custom)?);
                            }
                        }
                        "payload" => {
                            c.payload =
                                Some(m.next_value::<Box<RawValue>>()?.get().as_bytes().to_vec());
                        }
                        "version" | "sequence" => {
                            let _: Option<i64> = m.next_value()?;
                        }
                        _ => return Err(de::Error::custom("unknown field")),
                    }
                }
                Ok(())
            }
        }
        d.deserialize_any(ChangeVisitor(self.0))
    }
}

fn fold_key(s: &str) -> String {
    // Unicode simple folding in encoding/json also equates long s with S.
    s.chars()
        .flat_map(|c| if c == 'ſ' { 's' } else { c }.to_lowercase())
        .collect()
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, &'static str> {
    // Match Go's RFC3339 parser, including its currently enabled fallback:
    // one-digit hours, comma fractions, offsets up to 24:60, nanosecond
    // truncation, and rejection of leap seconds and lowercase t/z.
    let invalid = "invalid RFC3339 timestamp";
    if !s.is_ascii() {
        return Err(invalid);
    }
    let (date, time) = s.split_once('T').ok_or(invalid)?;
    let number = |v: &str, width: usize| -> Result<u32, &'static str> {
        if v.len() != width || !v.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid);
        }
        v.parse().map_err(|_| invalid)
    };
    if date.len() != 10 {
        return Err(invalid);
    }
    let mut date_parts = date.split('-');
    let year = number(date_parts.next().ok_or(invalid)?, 4)?;
    let month = number(date_parts.next().ok_or(invalid)?, 2)?;
    let day = number(date_parts.next().ok_or(invalid)?, 2)?;
    if date_parts.next().is_some() {
        return Err(invalid);
    }
    let (clock, offset) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0i64)
    } else {
        if time.len() < 6 {
            return Err(invalid);
        }
        let (clock, zone) = time.split_at(time.len() - 6);
        if !matches!(zone.as_bytes()[0], b'+' | b'-') || zone.as_bytes()[3] != b':' {
            return Err(invalid);
        }
        let hours = number(&zone[1..3], 2)?;
        let minutes = number(&zone[4..6], 2)?;
        if hours > 24 || minutes > 60 {
            return Err(invalid);
        }
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        (clock, sign * i64::from(hours * 3600 + minutes * 60))
    };
    let (clock, nanos) = if let Some(index) = clock.find(['.', ',']) {
        let fraction = &clock[index + 1..];
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid);
        }
        let precision = fraction.len().min(9);
        let nanos = number(&fraction[..precision], precision)? * 10u32.pow((9 - precision) as u32);
        (&clock[..index], nanos)
    } else {
        (clock, 0)
    };
    let mut parts = clock.split(':');
    let hour = parts.next().ok_or(invalid)?;
    if !(1..=2).contains(&hour.len()) {
        return Err(invalid);
    }
    let hour = number(hour, hour.len())?;
    let minute = number(parts.next().ok_or(invalid)?, 2)?;
    let second = number(parts.next().ok_or(invalid)?, 2)?;
    if parts.next().is_some() || second > 59 {
        return Err(invalid);
    }
    let local = NaiveDate::from_ymd_opt(year as i32, month, day)
        .and_then(|d| d.and_hms_nano_opt(hour, minute, second, nanos))
        .ok_or(invalid)?;
    local
        .and_utc()
        .checked_sub_signed(Duration::seconds(offset))
        .ok_or(invalid)
}

fn timestamp(t: DateTime<Utc>) -> String {
    // SecondsFormat::AutoSi pads to 3/6/9 digits, whereas Go strips every zero.
    let s = t.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let s = s
        .trim_end_matches('Z')
        .trim_end_matches('0')
        .trim_end_matches('.');
    format!("{s}Z")
}

fn failure(status: u16, code: &'static str, message: &str) -> SyncError {
    (status, code, message.to_owned())
}

/// Entities accepted by `/v1/sync` (contract v2, section 1).
pub const ENTITIES: [&str; 11] = [
    "transaction",
    "category",
    "budget",
    "account",
    "transfer",
    "recurring",
    "goal",
    "debt",
    "rate",
    "setting",
    "attachment",
];

/// A data space: the user's personal records (v1 tables) or a shared ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Space {
    User(String),
    Ledger(String),
}

impl Space {
    /// Records table, changes table and the column that selects the space.
    pub fn tables(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Space::User(_) => ("records", "changes", "user_id"),
            Space::Ledger(_) => ("ledger_records", "ledger_changes", "ledger_id"),
        }
    }
    pub fn id(&self) -> &str {
        match self {
            Space::User(id) | Space::Ledger(id) => id,
        }
    }
    /// Stable key of the space in `attachment_blobs`.
    pub fn key(&self) -> String {
        match self {
            Space::User(id) => format!("user:{id}"),
            Space::Ledger(id) => format!("ledger:{id}"),
        }
    }
}

/// Synchronize the personal space of `user_id`.
#[cfg(test)]
pub fn synchronize_wire(
    db: &mut Connection,
    user_id: &str,
    body: &[u8],
) -> Result<Vec<u8>, SyncError> {
    synchronize_space(db, &Space::User(user_id.to_owned()), body).map(|(response, _)| response)
}

/// Apply a sync request in `space`. Besides the response body, it returns the
/// attachment blobs that became garbage; their rows are already deleted in
/// the committed transaction and the caller removes the files.
pub fn synchronize_space(
    db: &mut Connection,
    space: &Space,
    body: &[u8],
) -> Result<(Vec<u8>, Vec<String>), SyncError> {
    let invalid_json = || failure(400, "invalid_json", "Request body is not valid JSON");
    let body = &body[..body.len().min(2 << 20)];
    // The original decoder reads the first JSON value, without requiring EOF.
    let (masked, raw_payloads) =
        crate::go_json::extract_raw_fields(body, "payload").map_err(|_| invalid_json())?;
    let normalized = crate::go_json::normalize(&masked);
    let mut request = Request::deserialize(&mut serde_json::Deserializer::from_slice(&normalized))
        .map_err(|_| invalid_json())?;
    for change in &mut request.changes {
        if let Some(placeholder) = &change.payload {
            let index: String = serde_json::from_slice(placeholder).map_err(|_| invalid_json())?;
            let index: usize = index.parse().map_err(|_| invalid_json())?;
            change.payload = Some(raw_payloads.get(index).ok_or_else(invalid_json)?.clone());
        }
    }
    if request.cursor < 0 || request.changes.len() > 5000 {
        return Err(failure(
            400,
            "invalid_sync_request",
            "Cursor or change count is invalid",
        ));
    }
    for c in &request.changes {
        let message = if !ENTITIES.contains(&c.entity.as_str()) {
            // The message is part of the recorded v1 reference responses
            // (tools/compatibility_expected.txt), so it is kept unchanged.
            Some("entity must be transaction, category, or budget")
        } else if c.id.is_empty() || c.id.len() > 128 {
            Some("record id is invalid")
        } else if c.updated_at.is_none() || c.updated_at == parse_time("0001-01-01T00:00:00Z").ok()
        {
            Some("updatedAt is required")
        } else if c
            .updated_at
            .is_some_and(|time| time > Utc::now() + Duration::hours(24))
        {
            Some("updatedAt is too far in the future")
        } else if !c.deleted && c.payload.is_none() {
            Some("payload is required for active records")
        } else if c.payload.as_ref().is_some_and(|p| p.len() > 256 * 1024) {
            Some("payload is too large")
        } else {
            None
        };
        if let Some(message) = message {
            return Err(failure(400, "invalid_change", message));
        }
    }
    let tx = db
        .transaction()
        .map_err(|_| failure(500, "sync_failed", "Could not begin synchronization"))?;
    let touches_attachments = request.changes.iter().any(|c| c.entity == "attachment");
    for c in request.changes {
        apply(&tx, space, c).map_err(|_| failure(500, "sync_failed", "Could not apply changes"))?;
    }
    let garbage = if touches_attachments {
        crate::attachments::collect_garbage(&tx, space, Utc::now())
            .map_err(|_| failure(500, "sync_failed", "Could not apply changes"))?
    } else {
        Vec::new()
    };
    let (changes, cursor, more) = if request.pull.unwrap_or(true) {
        load(&tx, space, request.cursor)
            .map_err(|_| failure(500, "sync_failed", "Could not load changes"))?
    } else {
        (Vec::new(), request.cursor, false)
    };
    tx.commit()
        .map_err(|_| failure(500, "sync_failed", "Could not complete synchronization"))?;
    let mut response = format!("{{\"cursor\":{cursor},\"changes\":[").into_bytes();
    for (index, change) in changes.iter().enumerate() {
        if index != 0 {
            response.push(b',');
        }
        response.extend_from_slice(change);
    }
    response.extend_from_slice(
        format!(
            "],\"hasMore\":{more},\"serverTime\":{}}}\n",
            serde_json::to_string(&timestamp(Utc::now())).expect("timestamp JSON")
        )
        .as_bytes(),
    );
    Ok((response, garbage))
}

#[cfg(test)]
fn synchronize(db: &mut Connection, user_id: &str, body: &[u8]) -> Result<Value, SyncError> {
    let response = synchronize_wire(db, user_id, body)?;
    serde_json::from_slice(&crate::go_json::normalize(&response))
        .map_err(|_| failure(500, "sync_failed", "Could not serialize synchronization"))
}

type InternalError = Box<dyn std::error::Error + Send + Sync>;

fn apply(tx: &Transaction<'_>, space: &Space, c: Change) -> Result<(), InternalError> {
    let (records, changes, key) = space.tables();
    let user = space.id();
    let current: Option<(String, i64)> = tx
        .query_row(
            &format!("SELECT updated_at,version FROM {records} WHERE {key}=? AND entity=? AND record_id=?"),
            params![user, c.entity, c.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let updated = c.updated_at.expect("validated timestamp");
    let mut version = 1;
    if let Some((time, old_version)) = current {
        if updated <= parse_time(&time)? {
            return Ok(());
        }
        version = old_version.wrapping_add(1);
    }
    let payload = if c.deleted {
        None
    } else {
        c.payload.as_deref()
    };
    let time = timestamp(updated);
    tx.execute(&format!("INSERT INTO {records}({key},entity,record_id,payload,updated_at,deleted,version) VALUES(?,?,?,CAST(? AS TEXT),?,?,?) ON CONFLICT({key},entity,record_id) DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at,deleted=excluded.deleted,version=excluded.version"), params![user,c.entity,c.id,payload,time,c.deleted,version])?;
    tx.execute(&format!("INSERT INTO {changes}({key},entity,record_id,payload,updated_at,deleted,version) VALUES(?,?,?,CAST(? AS TEXT),?,?,?)"), params![user,c.entity,c.id,payload,time,c.deleted,version])?;
    Ok(())
}

fn load(
    tx: &Transaction<'_>,
    space: &Space,
    after: i64,
) -> Result<(Vec<Vec<u8>>, i64, bool), InternalError> {
    let (_, changes, key) = space.tables();
    let user = space.id();
    let mut stmt = tx.prepare(&format!("SELECT sequence,entity,record_id,payload,updated_at,deleted,version FROM {changes} WHERE {key}=? AND sequence>? ORDER BY sequence ASC LIMIT ?"))?;
    let mut rows = stmt.query(params![user, after, 501])?;
    let mut result = Vec::new();
    let mut cursor = after;
    let mut more = false;
    while let Some(r) = rows.next()? {
        if result.len() == 500 {
            more = true;
            break;
        }
        let sequence: i64 = r.get(0)?;
        let payload: Option<Vec<u8>> = match r.get_ref(3)? {
            rusqlite::types::ValueRef::Null => None,
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                Some(bytes.to_vec())
            }
            _ => return Err("invalid database payload".into()),
        };
        let time: String = r.get(4)?;
        let version: i64 = r.get(6)?;
        let mut item = json!({"entity": r.get::<_, String>(1)?, "id": r.get::<_, String>(2)?, "updatedAt": timestamp(parse_time(&time)?), "deleted": r.get::<_, bool>(5)?});
        if sequence != 0 {
            item["sequence"] = json!(sequence);
        }
        if version != 0 {
            item["version"] = json!(version);
        }
        let mut encoded = serde_json::to_vec(&item)?;
        if let Some(p) = payload {
            serde_json::from_slice::<Box<RawValue>>(&crate::go_json::normalize(&p))?;
            encoded.pop();
            encoded.extend_from_slice(b",\"payload\":");
            encoded.extend_from_slice(&crate::go_json::compact_raw(&p));
            encoded.push(b'}');
        }
        result.push(encoded);
        cursor = sequence;
    }
    Ok((result, cursor, more))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let db = crate::database::open(":memory:").unwrap();
        db.execute(
            "INSERT INTO users VALUES('u','u@example.com','hash','now')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO users VALUES('v','v@example.com','hash','now')",
            [],
        )
        .unwrap();
        db
    }
    fn change(id: &str, time: &str) -> Value {
        json!({"entity":"category","id":id,"updatedAt":time,"payload":{"name":"Food"}})
    }
    fn sync(db: &mut Connection, body: Value) -> Result<Value, SyncError> {
        synchronize(db, "u", &serde_json::to_vec(&body).unwrap())
    }

    #[test]
    fn last_write_wins_and_replay_does_not_advance_sequence() {
        let mut db = db();
        let original = change("c", "2025-01-01T01:00:00+01:00");
        let first = sync(&mut db, json!({"changes":[original.clone()]})).unwrap();
        assert_eq!(first["cursor"], 1);
        assert_eq!(first["changes"][0]["updatedAt"], "2025-01-01T00:00:00Z");
        for c in [original, change("c", "2024-01-01T00:00:00Z")] {
            let r = sync(&mut db, json!({"cursor":1,"changes":[c]})).unwrap();
            assert_eq!(r["cursor"], 1);
            assert_eq!(r["changes"], json!([]));
        }
        let deleted = json!({"entity":"category","id":"c","updatedAt":"2025-02-01T00:00:00.123400000Z","deleted":true,"payload":{"ignored":true}});
        let r = sync(&mut db, json!({"cursor":1,"changes":[deleted]})).unwrap();
        assert_eq!(r["changes"][0]["version"], 2);
        assert_eq!(r["changes"][0]["deleted"], true);
        assert!(r["changes"][0].get("payload").is_none());
        assert_eq!(r["changes"][0]["updatedAt"], "2025-02-01T00:00:00.1234Z");
    }

    #[test]
    fn pagination_and_user_isolation_preserve_global_sequence() {
        let mut db = db();
        let changes: Vec<Value> = (0..501)
            .map(|i| change(&i.to_string(), "2025-01-01T00:00:00Z"))
            .collect();
        let r = sync(&mut db, json!({"changes":changes,"pull":false,"cursor":7})).unwrap();
        assert_eq!(r["cursor"], 7);
        assert_eq!(r["changes"], json!([]));
        let r = sync(&mut db, json!({})).unwrap();
        assert_eq!(r["changes"].as_array().unwrap().len(), 500);
        assert_eq!(r["cursor"], 500);
        assert_eq!(r["hasMore"], true);
        let r = sync(&mut db, json!({"cursor":500})).unwrap();
        assert_eq!(r["changes"].as_array().unwrap().len(), 1);
        assert_eq!(r["cursor"], 501);
        assert_eq!(r["hasMore"], false);
        let r = synchronize(&mut db, "v", b"{}").unwrap();
        assert_eq!(r["changes"], json!([]));
        assert_eq!(r["cursor"], 0);
    }

    #[test]
    fn all_pushes_roll_back_on_apply_or_pull_failure() {
        let mut db = db();
        db.execute_batch("CREATE TRIGGER reject_bad BEFORE INSERT ON records WHEN NEW.record_id='bad' BEGIN SELECT RAISE(ABORT, 'reject'); END;").unwrap();
        let r = sync(
            &mut db,
            json!({"changes":[change("good","2025-01-01T00:00:00Z"),change("bad","2025-01-01T00:00:00Z")]}),
        );
        assert_eq!(r.unwrap_err().2, "Could not apply changes");
        assert_eq!(
            db.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute("INSERT INTO changes(user_id,entity,record_id,updated_at,deleted,version) VALUES('u','category','broken','invalid-time',1,1)", []).unwrap();
        assert_eq!(
            sync(
                &mut db,
                json!({"changes":[change("good","2025-01-01T00:00:00Z")]})
            )
            .unwrap_err()
            .2,
            "Could not load changes"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn strict_fields_and_go_null_defaults_and_validation() {
        let mut db = db();
        for raw in [
            b"null".as_slice(),
            b"{\"cursor\":null,\"deviceId\":null,\"changes\":null,\"pull\":null}",
            b"{} {}",
        ] {
            assert!(synchronize(&mut db, "u", raw).is_ok());
        }
        for raw in [
            b"{\"extra\":1}".as_slice(),
            b"{\"changes\":[{\"extra\":1}]}",
            b"{\"cursor\":1.0}",
            b"{\"changes\":{}}",
        ] {
            assert_eq!(
                synchronize(&mut db, "u", raw).unwrap_err().1,
                "invalid_json"
            );
        }
        let raw = br#"{"CHANGES":[{"ENTITY":"category","ID":"c","updatedAt":"2025-01-01T00:00:00Z","payload":null}]}"#;
        let r = synchronize(&mut db, "u", raw).unwrap();
        assert!(r["changes"][0].get("payload").unwrap().is_null());
        assert_eq!(
            sync(&mut db, json!({"cursor":-1})).unwrap_err().1,
            "invalid_sync_request"
        );
        assert_eq!(
            sync(&mut db, json!({"changes":[null]})).unwrap_err().2,
            "entity must be transaction, category, or budget"
        );
        let mut c = change("c", "0001-01-01T00:00:00Z");
        assert_eq!(
            sync(&mut db, json!({"changes":[c.clone()]})).unwrap_err().2,
            "updatedAt is required"
        );
        c["updatedAt"] = json!(timestamp(Utc::now() + Duration::hours(25)));
        assert_eq!(
            sync(&mut db, json!({"changes":[c]})).unwrap_err().2,
            "updatedAt is too far in the future"
        );
        let mut c = change("c", "2025-01-01T00:00:00Z");
        c["payload"] = json!("x".repeat(256 * 1024));
        assert_eq!(
            sync(&mut db, json!({"changes":[c]})).unwrap_err().2,
            "payload is too large"
        );
    }
    #[test]
    fn go_timestamp_parser_accepts_fallbacks_and_rejects_chrono_extensions() {
        for (input, expected) in [
            ("2025-01-02T3:04:05Z", "2025-01-02T03:04:05Z"),
            (
                "2025-01-02T03:04:05,123456789123Z",
                "2025-01-02T03:04:05.123456789Z",
            ),
            ("2025-01-02T03:04:05+24:60", "2025-01-01T02:04:05Z"),
            ("0000-01-01T00:00:00Z", "0000-01-01T00:00:00Z"),
        ] {
            assert_eq!(timestamp(parse_time(input).unwrap()), expected, "{input}");
        }
        for input in [
            "2025-01-02t03:04:05Z",
            "2025-01-02T03:04:05z",
            "2025-01-02T03:04:60Z",
            "2025-01-02 03:04:05Z",
            "2025-01-02T03:04:05+25:00",
            "2025-01-02T03:04:05+01:61",
            "2025-01-02T03:04:05.Z",
            "2025-02-29T00:00:00Z",
        ] {
            assert!(parse_time(input).is_err(), "{input}");
        }
        let mut db = db();
        let raw = br#"{"changes":[{"entity":"category","id":"c","updatedAt":"\u0032025-01-01T00:00:00Z","payload":null}]}"#;
        assert_eq!(
            synchronize(&mut db, "u", raw).unwrap_err().1,
            "invalid_json"
        );
    }

    #[test]
    fn duplicate_fields_reuse_go_slice_elements_and_null_keeps_scalars() {
        let mut db = db();
        let raw = br#"{"changes":[{"entity":"category","id":"first","updatedAt":"2025-01-01T00:00:00Z","payload":null}],"changes":[{"id":"second","entity":null,"updatedAt":null}]}"#;
        let result = synchronize(&mut db, "u", raw).unwrap();
        assert_eq!(result["changes"][0]["id"], "second");
        assert_eq!(result["changes"][0]["entity"], "category");
        let raw = r#"{"curſor":1,"CHANGES":[{"entity":"category","id":"third","updatedAt":"2025-01-02T00:00:00Z","payload":true}]}"#;
        let result = synchronize(&mut db, "u", raw.as_bytes()).unwrap();
        assert_eq!(result["changes"][0]["id"], "third");
        assert_eq!(result["changes"][0]["payload"], true);
    }

    #[test]
    fn exact_payload_numbers_and_database_raw_text_are_preserved() {
        let mut db = db();
        let payload = r#"{ "large": 123456789012345678901234567890, "decimal": 0.123456789012345678901234567890, "exponent": 1e999 }"#;
        let body = format!(
            r#"{{"changes":[{{"entity":"transaction","id":"t","updatedAt":"2025-01-01T00:00:00Z","payload":{payload}}}]}}"#
        );
        let result = synchronize(&mut db, "u", body.as_bytes()).unwrap();
        let stored: String = db
            .query_row("SELECT payload FROM records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, payload);
        let serialized = serde_json::to_string(&result["changes"][0]["payload"]).unwrap();
        assert!(serialized.contains("123456789012345678901234567890"));
        assert!(serialized.contains("0.123456789012345678901234567890"));
        assert!(serialized.to_lowercase().contains("1e+999") || serialized.contains("1e999"));
    }

    #[test]
    fn validation_limits_use_bytes_and_validate_whole_batch_before_writing() {
        let mut db = db();
        let mut invalid = change(&"я".repeat(65), "2025-01-01T00:00:00Z");
        assert_eq!(
            sync(
                &mut db,
                json!({"changes":[change("good","2025-01-01T00:00:00Z"),invalid.clone()]})
            )
            .unwrap_err()
            .2,
            "record id is invalid"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        invalid["id"] = json!("я".repeat(64));
        assert!(sync(&mut db, json!({"changes":[invalid]})).is_ok());
        let changes: Vec<_> = (0..5001)
            .map(|_| change("c", "2025-01-01T00:00:00Z"))
            .collect();
        assert_eq!(
            sync(&mut db, json!({"changes":changes})).unwrap_err().1,
            "invalid_sync_request"
        );
        let raw = format!(
            r#"{{"changes":[{{"entity":"category","id":"large","updatedAt":"2025-01-01T00:00:00Z","payload":{{ {} }} }}]}}"#,
            " ".repeat(256 * 1024)
        );
        assert_eq!(
            synchronize(&mut db, "u", raw.as_bytes()).unwrap_err().2,
            "payload is too large"
        );
        let mut trailing = b"{} ".to_vec();
        trailing.resize((2 << 20) + 100, b' ');
        assert!(synchronize(&mut db, "u", &trailing).is_ok());
    }

    #[test]
    fn wire_preserves_raw_payload_duplicates_unicode_escapes_and_invalid_utf8() {
        let mut db = db();
        let payload = b"{ \"duplicate\":1, \"duplicate\":2, \"surrogate\":\"\\ud800\", \"invalid\":\"\xff\", \"html\":\"<&>\" }";
        let mut body = br#"{"changes":[{"entity":"category","id":"\ud800","updatedAt":"2025-01-01T00:00:00Z","payload":"#.to_vec();
        body.extend_from_slice(payload);
        body.extend_from_slice(b"}]}");
        let response = synchronize_wire(&mut db, "u", &body).unwrap();
        let id = "\"id\":\"�\"".as_bytes();
        assert!(response.windows(id.len()).any(|w| w == id));
        let expected = b"\"payload\":{\"duplicate\":1,\"duplicate\":2,\"surrogate\":\"\\ud800\",\"invalid\":\"\xff\",\"html\":\"\\u003c\\u0026\\u003e\"}";
        assert!(response.windows(expected.len()).any(|w| w == expected));
        let stored: Vec<u8> = db
            .query_row("SELECT CAST(payload AS BLOB) FROM records", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, payload);
        let storage_type: String = db
            .query_row("SELECT typeof(payload) FROM records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(storage_type, "text");
        let pulled = synchronize_wire(&mut db, "u", b"{}").unwrap();
        assert!(pulled.windows(expected.len()).any(|w| w == expected));
    }

    #[test]
    fn raw_payload_limits_count_original_bytes_before_unicode_replacement() {
        let mut db = db();
        let mut body = br#"{"changes":[{"entity":"category","id":"c","updatedAt":"2025-01-01T00:00:00Z","payload":""#.to_vec();
        body.extend(std::iter::repeat_n(0xff, 256 * 1024 - 2));
        body.extend_from_slice(b"\"}]}");
        assert!(synchronize_wire(&mut db, "u", &body).is_ok());
        let body = b"{\"changes\":[{\"entity\":\"category\",\"id\":\"\xff\",\"updatedAt\":\"2025-01-02T00:00:00Z\",\"payload\":null}]}";
        assert!(synchronize_wire(&mut db, "u", body).is_ok());
    }

    #[test]
    fn tombstone_resurrection_and_sequential_batch_conflicts_keep_history() {
        let mut db = db();
        let initial = change("c", "2025-01-01T00:00:00Z");
        let tombstone =
            json!({"entity":"category","id":"c","updatedAt":"2025-01-02T00:00:00Z","deleted":true});
        let newer = change("c", "2025-01-03T00:00:00Z");
        let result = sync(
            &mut db,
            json!({"changes":[initial.clone(),tombstone.clone(),initial,newer.clone(),newer]}),
        )
        .unwrap();
        let changes = result["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0]["version"], 1);
        assert_eq!(changes[1]["version"], 2);
        assert_eq!(changes[2]["version"], 3);
        assert_eq!(changes[2]["deleted"], false);
        assert!(changes[1].get("payload").is_none());
        let isolated = synchronize(
            &mut db,
            "v",
            &serde_json::to_vec(&json!({"changes":[tombstone]})).unwrap(),
        )
        .unwrap();
        assert_eq!(isolated["cursor"], 4);
        let user = sync(&mut db, json!({"cursor":3})).unwrap();
        assert_eq!(user["cursor"], 3);
        assert_eq!(user["changes"], json!([]));
    }

    #[test]
    fn concurrent_device_retries_have_one_winner_and_unique_sequences() {
        use std::sync::{Arc, Barrier, Mutex};
        let db = Arc::new(Mutex::new(db()));
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for index in 0..8 {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let c = change("shared", &format!("2025-01-01T00:00:0{index}Z"));
                let mut db = db.lock().unwrap();
                sync(&mut db, json!({"changes":[c.clone(), c],"pull":false})).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let mut db = db.lock().unwrap();
        let (time, version): (String, i64) = db
            .query_row("SELECT updated_at,version FROM records", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(time, "2025-01-01T00:00:07Z");
        let result = sync(&mut db, json!({})).unwrap();
        let changes = result["changes"].as_array().unwrap();
        assert_eq!(changes.len() as i64, version);
        for (index, change) in changes.iter().enumerate() {
            assert_eq!(change["sequence"], (index + 1) as i64);
            assert_eq!(change["version"], (index + 1) as i64);
        }
    }
}
