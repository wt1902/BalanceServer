//! Endpoint tests for contract v2: capabilities, ledgers and attachments.

use super::*;
use sha2::{Digest, Sha256};

struct Env {
    state: State,
    dir: std::path::PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env() -> Env {
    let dir = std::env::temp_dir().join(format!("balance-v2-test-{}", auth::identifier().unwrap()));
    let state = State::new(
        Config {
            address: "127.0.0.1:0".into(),
            database_path: ":memory:".into(),
            jwt_secret: "test secret".into(),
            access_ttl: chrono::TimeDelta::minutes(15),
            refresh_ttl: chrono::TimeDelta::days(30),
            allow_registration: true,
            cors_origins: vec!["https://allowed.example".into()],
            attachments_dir: dir.clone(),
        },
        Mutex::new(crate::database::open(":memory:").unwrap()),
    );
    {
        let db = state.db.lock().unwrap();
        for user in ["owner", "member", "stranger"] {
            db.execute(
                "INSERT INTO users VALUES(?,?,'hash','now')",
                params![user, format!("{user}@example.com")],
            )
            .unwrap();
        }
    }
    Env { state, dir }
}

fn auth_headers(env: &Env, user: &str, ledger: Option<&str>) -> HeaderMap {
    let (token, _) = env
        .state
        .tokens
        .access_token(user, &format!("{user}@example.com"))
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    if let Some(ledger) = ledger {
        headers.insert("X-Balance-Ledger", HeaderValue::from_str(ledger).unwrap());
    }
    headers
}

fn call(
    env: &Env,
    method: &str,
    path: &str,
    user: Option<&str>,
    ledger: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    let headers = match user {
        Some(user) => auth_headers(env, user, ledger),
        None => HeaderMap::new(),
    };
    request(env, method, path, &headers, body)
}

fn request(env: &Env, method: &str, path: &str, headers: &HeaderMap, body: &[u8]) -> (u16, Value) {
    let r = dispatch(
        &env.state,
        &Method::from_bytes(method.as_bytes()).unwrap(),
        path,
        headers,
        Ok(body),
        "test-host",
    );
    let status = r.status().as_u16();
    let bytes = futures_body(r);
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn futures_body(r: Response) -> Vec<u8> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(axum::body::to_bytes(r.into_body(), usize::MAX))
        .unwrap()
        .to_vec()
}

fn code_of(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn create_ledger(env: &Env) -> String {
    let (status, v) = call(
        env,
        "POST",
        "/v1/ledgers",
        Some("owner"),
        None,
        br#"{"name":" Family "}"#,
    );
    assert_eq!(status, 201);
    v["id"].as_str().unwrap().to_owned()
}

fn join_member(env: &Env, ledger: &str) {
    let (_, invite) = call(
        env,
        "POST",
        &format!("/v1/ledgers/{ledger}/invites"),
        Some("owner"),
        None,
        b"",
    );
    let body = json!({"code": invite["code"]}).to_string();
    let (status, _) = call(
        env,
        "POST",
        "/v1/ledgers/join",
        Some("member"),
        None,
        body.as_bytes(),
    );
    assert_eq!(status, 200);
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn put(
    env: &Env,
    user: &str,
    ledger: Option<&str>,
    hash: &str,
    ty: &str,
    bytes: &[u8],
) -> (u16, Value) {
    let mut headers = auth_headers(env, user, ledger);
    headers.insert("Content-Type", HeaderValue::from_str(ty).unwrap());
    request(
        env,
        "PUT",
        &format!("/v1/attachments/{hash}"),
        &headers,
        bytes,
    )
}

fn get_raw(env: &Env, user: &str, ledger: Option<&str>, hash: &str) -> Response {
    dispatch(
        &env.state,
        &Method::GET,
        &format!("/v1/attachments/{hash}"),
        &auth_headers(env, user, ledger),
        Ok(b""),
        "test-host",
    )
}

fn sync_call(env: &Env, user: &str, ledger: Option<&str>, body: Value) -> (u16, Value) {
    call(
        env,
        "POST",
        "/v1/sync",
        Some(user),
        ledger,
        body.to_string().as_bytes(),
    )
}

fn attachment_change(id: &str, hash: &str, time: &str, deleted: bool) -> Value {
    json!({"entity":"attachment","id":id,"updatedAt":time,"deleted":deleted,
           "payload":{"transactionId":"t","sha256":hash,"contentType":"image/png","size":3,"name":"a.png","createdAt":time}})
}

#[test]
fn capabilities_are_public_and_match_contract() {
    let env = env();
    let (status, v) = call(&env, "GET", "/v1/capabilities", None, None, b"");
    assert_eq!(status, 200);
    assert_eq!(
        v,
        json!({
            "contract": 2,
            "entities": ["transaction","category","budget","account","transfer",
                         "recurring","goal","debt","rate","setting","attachment"],
            "attachments": {"maxBytes": 10485760,
                            "types": ["image/jpeg","image/png","image/heic","application/pdf"]},
            "ledgers": true
        })
    );
    let r = dispatch(
        &env.state,
        &Method::POST,
        "/v1/capabilities",
        &HeaderMap::new(),
        Ok(b""),
        "h",
    );
    assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(r.headers()["Allow"], "GET, HEAD");
}

#[test]
fn sync_accepts_every_v2_entity_and_still_rejects_unknown_ones() {
    let env = env();
    let changes: Vec<Value> = sync::ENTITIES
        .iter()
        .map(|e| json!({"entity":e,"id":format!("{e}-1"),"updatedAt":"2026-01-01T00:00:00Z","payload":{"x":1}}))
        .collect();
    let (status, v) = sync_call(&env, "owner", None, json!({ "changes": changes }));
    assert_eq!(status, 200);
    assert_eq!(v["changes"].as_array().unwrap().len(), 11);
    let (status, v) = sync_call(
        &env,
        "owner",
        None,
        json!({"changes":[{"entity":"wallet","id":"w","updatedAt":"2026-01-01T00:00:00Z","payload":{}}]}),
    );
    assert_eq!(status, 400);
    assert_eq!(code_of(&v), "invalid_change");
}

#[test]
fn v2_endpoints_require_authorization() {
    let env = env();
    for (method, path) in [
        ("GET", "/v1/ledgers"),
        ("POST", "/v1/ledgers"),
        ("POST", "/v1/ledgers/join"),
        ("PATCH", "/v1/ledgers/x"),
        ("DELETE", "/v1/ledgers/x"),
        ("POST", "/v1/ledgers/x/invites"),
        ("DELETE", "/v1/ledgers/x/members/y"),
        ("GET", "/v1/attachments/x"),
        ("PUT", "/v1/attachments/x"),
    ] {
        let (status, v) = call(&env, method, path, None, None, b"{}");
        assert_eq!(status, 401, "{method} {path}");
        assert_eq!(code_of(&v), "authorization_required");
    }
    let (status, _) = call(&env, "PUT", "/v1/ledgers", Some("owner"), None, b"");
    assert_eq!(status, 405);
    let (status, _) = call(&env, "GET", "/v1/ledgers/join", Some("owner"), None, b"");
    assert_eq!(status, 405);
}

#[test]
fn ledger_lifecycle_roles_and_error_codes() {
    let env = env();
    // Personal space is not listed.
    let (status, v) = call(&env, "GET", "/v1/ledgers", Some("owner"), None, b"");
    assert_eq!((status, v), (200, json!({"ledgers": []})));
    for bad in [&br#"{"name":"   "}"#[..], br#"{}"#] {
        let (status, v) = call(&env, "POST", "/v1/ledgers", Some("owner"), None, bad);
        assert_eq!((status, code_of(&v)), (400, "invalid_name"));
    }
    let (status, v) = call(&env, "POST", "/v1/ledgers", Some("owner"), None, b"{");
    assert_eq!((status, code_of(&v)), (400, "invalid_json"));

    let id = create_ledger(&env);
    let (_, v) = call(&env, "GET", "/v1/ledgers", Some("owner"), None, b"");
    assert_eq!(
        v,
        json!({"ledgers":[{"id":id,"name":"Family","role":"owner",
            "members":[{"userId":"owner","email":"owner@example.com","role":"owner"}]}]})
    );
    let ledger = format!("/v1/ledgers/{id}");
    let invites = format!("{ledger}/invites");

    // Unknown ledger → 404, non-member → 403.
    let (status, v) = call(
        &env,
        "PATCH",
        "/v1/ledgers/missing",
        Some("owner"),
        None,
        br#"{"name":"x"}"#,
    );
    assert_eq!((status, code_of(&v)), (404, "not_found"));
    let (status, v) = call(
        &env,
        "PATCH",
        &ledger,
        Some("stranger"),
        None,
        br#"{"name":"x"}"#,
    );
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    let (status, v) = call(&env, "POST", &invites, Some("stranger"), None, b"");
    assert_eq!((status, code_of(&v)), (403, "forbidden"));

    let (status, v) = call(
        &env,
        "PATCH",
        &ledger,
        Some("owner"),
        None,
        br#"{"name":"Home"}"#,
    );
    assert_eq!((status, v["name"].as_str()), (200, Some("Home")));

    // Invites: 10 characters, 7 days, hashed at rest, single use.
    let (status, invite) = call(&env, "POST", &invites, Some("owner"), None, b"");
    assert_eq!(status, 201);
    let code = invite["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 10);
    let expires = DateTime::parse_from_rfc3339(invite["expiresAt"].as_str().unwrap()).unwrap();
    let days = (expires.with_timezone(&Utc) - Utc::now()).num_hours();
    assert!((167..=168).contains(&days), "{days}");
    {
        let db = env.state.db.lock().unwrap();
        let stored: String = db
            .query_row("SELECT code_hash FROM ledger_invites", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, code);
        assert_eq!(stored, auth::token_hash(&code));
    }
    for bad in [
        json!({"code":"AAAAAAAAAA"}),
        json!({"code":"short"}),
        json!({}),
    ] {
        let (status, v) = call(
            &env,
            "POST",
            "/v1/ledgers/join",
            Some("member"),
            None,
            bad.to_string().as_bytes(),
        );
        assert_eq!((status, code_of(&v)), (400, "invalid_code"));
    }
    let lower = json!({"code": code.to_lowercase()}).to_string();
    let (status, v) = call(
        &env,
        "POST",
        "/v1/ledgers/join",
        Some("member"),
        None,
        lower.as_bytes(),
    );
    assert_eq!(status, 200);
    assert_eq!(v["role"], "member");
    assert_eq!(v["members"].as_array().unwrap().len(), 2);
    let (status, v) = call(
        &env,
        "POST",
        "/v1/ledgers/join",
        Some("stranger"),
        None,
        lower.as_bytes(),
    );
    assert_eq!((status, code_of(&v)), (400, "invalid_code"));

    // Expired codes are rejected.
    let (_, expired) = call(&env, "POST", &invites, Some("owner"), None, b"");
    env.state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE ledger_invites SET expires_at='2000-01-01T00:00:00.000000000Z' WHERE code_hash=?",
            [auth::token_hash(expired["code"].as_str().unwrap())],
        )
        .unwrap();
    let body = json!({"code": expired["code"]}).to_string();
    let (status, v) = call(
        &env,
        "POST",
        "/v1/ledgers/join",
        Some("stranger"),
        None,
        body.as_bytes(),
    );
    assert_eq!((status, code_of(&v)), (400, "invalid_code"));

    // Members may not administer the ledger.
    let (status, v) = call(
        &env,
        "PATCH",
        &ledger,
        Some("member"),
        None,
        br#"{"name":"x"}"#,
    );
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    let (status, v) = call(&env, "POST", &invites, Some("member"), None, b"");
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    let (status, v) = call(&env, "DELETE", &ledger, Some("member"), None, b"");
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    let (status, v) = call(
        &env,
        "DELETE",
        &format!("{ledger}/members/owner"),
        Some("member"),
        None,
        b"",
    );
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    let (_, v) = call(&env, "GET", "/v1/ledgers", Some("member"), None, b"");
    assert_eq!(v["ledgers"][0]["role"], "member");

    // The owner cannot leave; unknown members are 404.
    let (status, v) = call(
        &env,
        "DELETE",
        &format!("{ledger}/members/owner"),
        Some("owner"),
        None,
        b"",
    );
    assert_eq!((status, code_of(&v)), (409, "owner_cannot_leave"));
    let (status, v) = call(
        &env,
        "DELETE",
        &format!("{ledger}/members/stranger"),
        Some("owner"),
        None,
        b"",
    );
    assert_eq!((status, code_of(&v)), (404, "not_found"));

    // A member leaves; the owner removes a member.
    let (status, _) = call(
        &env,
        "DELETE",
        &format!("{ledger}/members/member"),
        Some("member"),
        None,
        b"",
    );
    assert_eq!(status, 204);
    let (_, v) = call(&env, "GET", "/v1/ledgers", Some("member"), None, b"");
    assert_eq!(v, json!({"ledgers": []}));
    join_member(&env, &id);
    let (status, _) = call(
        &env,
        "DELETE",
        &format!("{ledger}/members/member"),
        Some("owner"),
        None,
        b"",
    );
    assert_eq!(status, 204);
    let (status, v) = sync_call(&env, "member", Some(&id), json!({}));
    assert_eq!((status, code_of(&v)), (403, "forbidden"));

    let (status, _) = call(&env, "DELETE", &ledger, Some("owner"), None, b"");
    assert_eq!(status, 204);
    let (status, v) = call(&env, "DELETE", &ledger, Some("owner"), None, b"");
    assert_eq!((status, code_of(&v)), (404, "not_found"));
    let db = env.state.db.lock().unwrap();
    for table in ["ledgers", "ledger_members", "ledger_invites"] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}

#[test]
fn joining_twice_keeps_the_code_and_the_role() {
    let env = env();
    let id = create_ledger(&env);
    let (_, invite) = call(
        &env,
        "POST",
        &format!("/v1/ledgers/{id}/invites"),
        Some("owner"),
        None,
        b"",
    );
    let body = json!({"code": invite["code"]}).to_string();
    let (status, v) = call(
        &env,
        "POST",
        "/v1/ledgers/join",
        Some("owner"),
        None,
        body.as_bytes(),
    );
    assert_eq!((status, v["role"].as_str()), (200, Some("owner")));
    let (status, v) = call(
        &env,
        "POST",
        "/v1/ledgers/join",
        Some("member"),
        None,
        body.as_bytes(),
    );
    assert_eq!((status, v["role"].as_str()), (200, Some("member")));
}

#[test]
fn join_attempts_are_rate_limited() {
    let env = env();
    for _ in 0..30 {
        let (status, _) = call(
            &env,
            "POST",
            "/v1/ledgers/join",
            Some("member"),
            None,
            br#"{"code":"x"}"#,
        );
        assert_eq!(status, 400);
    }
    let (status, v) = call(
        &env,
        "POST",
        "/v1/ledgers/join",
        Some("member"),
        None,
        br#"{"code":"x"}"#,
    );
    assert_eq!((status, code_of(&v)), (429, "rate_limited"));
}

#[test]
fn ledger_sync_is_shared_isolated_and_has_its_own_cursor() {
    let env = env();
    // Personal data first, so personal and ledger sequences differ.
    let personal = json!({"changes":[{"entity":"category","id":"p","updatedAt":"2026-01-01T00:00:00Z","payload":{"name":"Mine"}}]});
    let (_, v) = sync_call(&env, "owner", None, personal.clone());
    assert_eq!(v["cursor"], 1);
    let (_, v) = sync_call(&env, "owner", None, personal);
    assert_eq!(v["cursor"], 1);

    let id = create_ledger(&env);
    join_member(&env, &id);
    let shared = json!({"changes":[{"entity":"transaction","id":"t","updatedAt":"2026-01-02T00:00:00Z","payload":{"amount":10}}]});
    let (status, v) = sync_call(&env, "owner", Some(&id), shared);
    assert_eq!(status, 200);
    assert_eq!(v["cursor"], 1);
    assert_eq!(v["changes"][0]["id"], "t");

    // The member sees the ledger record and nothing personal of the owner.
    let (_, v) = sync_call(&env, "member", Some(&id), json!({}));
    assert_eq!(v["changes"].as_array().unwrap().len(), 1);
    assert_eq!(v["changes"][0]["entity"], "transaction");
    // Last write wins among members.
    let edit = json!({"cursor":1,"changes":[{"entity":"transaction","id":"t","updatedAt":"2026-01-03T00:00:00Z","payload":{"amount":20}}]});
    let (_, v) = sync_call(&env, "member", Some(&id), edit);
    assert_eq!(v["changes"][0]["version"], 2);
    assert_eq!(v["cursor"], 2);
    let (_, v) = sync_call(&env, "owner", Some(&id), json!({"cursor":1}));
    assert_eq!(v["changes"][0]["payload"]["amount"], 20);

    // Personal spaces stay untouched.
    let (_, v) = sync_call(&env, "member", None, json!({}));
    assert_eq!(
        (v["cursor"].clone(), v["changes"].clone()),
        (json!(0), json!([]))
    );
    let (_, v) = sync_call(&env, "owner", None, json!({}));
    assert_eq!(v["changes"].as_array().unwrap().len(), 1);
    assert_eq!(v["changes"][0]["id"], "p");
    // An empty header means the personal space.
    let (_, v) = sync_call(&env, "owner", Some(" "), json!({}));
    assert_eq!(v["changes"][0]["id"], "p");

    let (status, v) = sync_call(&env, "stranger", Some(&id), json!({}));
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    let (status, v) = sync_call(&env, "owner", Some("missing"), json!({}));
    assert_eq!((status, code_of(&v)), (404, "not_found"));

    // Deleting the ledger removes its records and changes.
    call(
        &env,
        "DELETE",
        &format!("/v1/ledgers/{id}"),
        Some("owner"),
        None,
        b"",
    );
    let db = env.state.db.lock().unwrap();
    for table in ["ledger_records", "ledger_changes"] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let n: i64 = db
        .query_row("SELECT count(*) FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn attachment_upload_validation_and_idempotency() {
    let env = env();
    let bytes = b"\x89PNG fake";
    let hash = sha(bytes);
    let (status, v) = put(&env, "owner", None, &hash, "image/png", bytes);
    assert_eq!(status, 201);
    assert_eq!(v["sha256"], hash.as_str());
    let (status, _) = put(&env, "owner", None, &hash, "image/png", bytes);
    assert_eq!(status, 200);

    let (status, v) = put(&env, "owner", None, &sha(b"other"), "image/png", bytes);
    assert_eq!((status, code_of(&v)), (400, "hash_mismatch"));
    let (status, v) = put(
        &env,
        "owner",
        None,
        &hash.to_uppercase(),
        "image/png",
        bytes,
    );
    assert_eq!((status, code_of(&v)), (400, "hash_mismatch"));
    let (status, v) = put(&env, "owner", None, &hash, "image/gif", bytes);
    assert_eq!((status, code_of(&v)), (415, "unsupported_media_type"));
    let mut headers = auth_headers(&env, "owner", None);
    let (status, v) = request(
        &env,
        "PUT",
        &format!("/v1/attachments/{hash}"),
        &headers,
        bytes,
    );
    assert_eq!((status, code_of(&v)), (415, "unsupported_media_type"));
    headers.insert("Content-Type", HeaderValue::from_static("application/pdf"));
    let large = vec![0u8; attachments::MAX_BYTES + 1];
    let (status, v) = request(
        &env,
        "PUT",
        &format!("/v1/attachments/{}", sha(&large)),
        &headers,
        &large,
    );
    assert_eq!((status, code_of(&v)), (413, "payload_too_large"));
    let max = vec![1u8; attachments::MAX_BYTES];
    let (status, _) = request(
        &env,
        "PUT",
        &format!("/v1/attachments/{}", sha(&max)),
        &headers,
        &max,
    );
    assert_eq!(status, 201);

    let r = get_raw(&env, "owner", None, &hash);
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["Content-Type"], "image/png");
    assert_eq!(r.headers()["Content-Disposition"], "attachment");
    assert_eq!(futures_body(r), bytes);
    // Other users' spaces do not see the file.
    assert_eq!(
        get_raw(&env, "member", None, &hash).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_raw(&env, "owner", None, "nothex").status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn attachments_are_per_ledger_and_check_membership() {
    let env = env();
    let id = create_ledger(&env);
    join_member(&env, &id);
    let bytes = b"%PDF-1.7 receipt";
    let hash = sha(bytes);
    assert_eq!(
        put(&env, "member", Some(&id), &hash, "application/pdf", bytes).0,
        201
    );
    let r = get_raw(&env, "owner", Some(&id), &hash);
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(futures_body(r), bytes);
    // The same file in the personal space is stored separately.
    assert_eq!(
        get_raw(&env, "owner", None, &hash).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        put(&env, "owner", None, &hash, "application/pdf", bytes).0,
        201
    );

    let (status, v) = put(&env, "stranger", Some(&id), &hash, "application/pdf", bytes);
    assert_eq!((status, code_of(&v)), (403, "forbidden"));
    assert_eq!(
        get_raw(&env, "stranger", Some(&id), &hash).status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_raw(&env, "owner", Some("missing"), &hash).status(),
        StatusCode::NOT_FOUND
    );

    // Ledger deletion removes its blobs, the personal copy stays.
    let ledger_dir = env.dir.join("ledgers").join(&id);
    assert!(ledger_dir.is_dir());
    call(
        &env,
        "DELETE",
        &format!("/v1/ledgers/{id}"),
        Some("owner"),
        None,
        b"",
    );
    assert!(!ledger_dir.exists());
    assert_eq!(get_raw(&env, "owner", None, &hash).status(), StatusCode::OK);
    let n: i64 = env
        .state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM attachment_blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn blobs_are_collected_when_no_live_record_references_them() {
    let env = env();
    let bytes = b"jpeg bytes";
    let hash = sha(bytes);
    let pending = b"uploaded, not yet synced";
    let pending_hash = sha(pending);
    assert_eq!(put(&env, "owner", None, &hash, "image/jpeg", bytes).0, 201);
    assert_eq!(
        put(&env, "owner", None, &pending_hash, "image/jpeg", pending).0,
        201
    );
    let file = env
        .dir
        .join("users")
        .join("owner")
        .join(&hash[..2])
        .join(&hash);
    assert!(file.is_file());

    let (status, _) = sync_call(
        &env,
        "owner",
        None,
        json!({"changes":[attachment_change("a1", &hash, "2026-01-01T00:00:00Z", false),
                          attachment_change("a2", &hash, "2026-01-01T00:00:00Z", false)]}),
    );
    assert_eq!(status, 200);
    // One of two referencing records is deleted: the blob stays.
    sync_call(
        &env,
        "owner",
        None,
        json!({"changes":[attachment_change("a1", &hash, "2026-01-02T00:00:00Z", true)]}),
    );
    assert_eq!(get_raw(&env, "owner", None, &hash).status(), StatusCode::OK);
    // The last one is deleted: the blob is collected.
    sync_call(
        &env,
        "owner",
        None,
        json!({"changes":[attachment_change("a2", &hash, "2026-01-02T00:00:00Z", true)]}),
    );
    assert_eq!(
        get_raw(&env, "owner", None, &hash).status(),
        StatusCode::NOT_FOUND
    );
    assert!(!file.exists());
    // A fresh upload whose record is not synced yet survives the grace period…
    assert_eq!(
        get_raw(&env, "owner", None, &pending_hash).status(),
        StatusCode::OK
    );
    // …but is collected once it is older than the grace period.
    env.state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE attachment_blobs SET uploaded_at='2000-01-01T00:00:00.000000000Z'",
            [],
        )
        .unwrap();
    sync_call(
        &env,
        "owner",
        None,
        json!({"changes":[attachment_change("a3", &hash, "2026-01-03T00:00:00Z", true)]}),
    );
    assert_eq!(
        get_raw(&env, "owner", None, &pending_hash).status(),
        StatusCode::NOT_FOUND
    );
    // Re-uploading a collected blob creates it again.
    assert_eq!(put(&env, "owner", None, &hash, "image/jpeg", bytes).0, 201);
}

#[tokio::test]
async fn large_uploads_pass_the_body_limit_only_when_authorized() {
    let env = env();
    let state = Arc::new(State::new(
        env.state.cfg.clone(),
        Mutex::new(crate::database::open(":memory:").unwrap()),
    ));
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO users VALUES('owner','owner@example.com','hash','now')",
            [],
        )
        .unwrap();
    let (token, _) = state
        .tokens
        .access_token("owner", "owner@example.com")
        .unwrap();
    let bytes = vec![7u8; (2 << 20) + 10];
    let hash = sha(&bytes);
    let r = handle(
        ExtractState(state.clone()),
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/attachments/{hash}"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "image/heic")
            .body(Body::from(bytes.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(r.headers()["Cache-Control"], "no-store");
    let r = handle(
        ExtractState(state.clone()),
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/attachments/{hash}"))
            .header("Content-Type", "image/heic")
            .body(Body::from(bytes))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cors_preflights_allow_v2_methods_and_ledger_header() {
    let env = env();
    let state = Arc::new(State::new(
        env.state.cfg.clone(),
        Mutex::new(crate::database::open(":memory:").unwrap()),
    ));
    for (uri, request_method) in [("/v1/ledgers/x", None), ("/v1/sync", Some("POST"))] {
        let mut builder = Request::builder()
            .method("OPTIONS")
            .uri(uri)
            .header("Origin", "https://allowed.example");
        if let Some(m) = request_method {
            builder = builder
                .header("Access-Control-Request-Method", m)
                .header("Access-Control-Request-Headers", "x-balance-ledger");
        }
        let r = handle(
            ExtractState(state.clone()),
            builder.body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            r.headers()["Access-Control-Allow-Headers"],
            "Authorization, Content-Type, X-Balance-Ledger"
        );
        assert_eq!(
            r.headers()["Access-Control-Allow-Methods"],
            "GET, POST, PUT, PATCH, DELETE, OPTIONS"
        );
    }
}
