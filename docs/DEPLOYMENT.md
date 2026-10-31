# Deployment

## Runtime and configuration

The production executable is the Rust server, built with `cargo build --locked
--release`. SQLite is bundled into the executable. The original Go implementation
was removed after v1.0.0; the `v1.0.0` tag still contains its sources for the
migration and rollback below (`go build -o balance-server-go ./cmd/balance-server`).

The Dockerfile uses a Rust 1.90 Debian Bookworm builder and a Debian Bookworm slim
runtime, running as the unprivileged `balance` user. The image embeds the existing
web client and uses no separate frontend process. Build dependencies and SQLite
are compiled at image build time.
The numeric identity remains UID 100 / GID 101, matching the original Alpine Go
image, so existing volume permissions are preserved across the switch.

The image is multi-platform: `linux/amd64`, `linux/arm64` and `linux/arm/v7`
(armhf) are published under the same tag, and `docker pull` selects the matching
one. The builder stage runs on the build machine's platform and cross-compiles
Rust (with GCC cross compilers for bundled SQLite), so only the runtime stage
needs QEMU. Build all platforms locally with a `docker-container` buildx builder:
`docker buildx build --platform linux/amd64,linux/arm64,linux/arm/v7 .`

`docker compose up -d --build` publishes **http://localhost:8123** (host 8123,
container 8080). `GET /health` is the liveness endpoint. Direct binary execution
uses `:8080` by default. Place a TLS reverse proxy in front of the published port
for remote access; pass `X-Forwarded-Proto: https` to enable Secure session cookies.

| Variable | Default | Purpose |
| --- | --- | --- |
| `BALANCE_ADDR` | `:8080` | HTTP listen address |
| `BALANCE_DB_PATH` | `./data/balance.db` | SQLite file; Docker uses `/data/balance.db` |
| `BALANCE_JWT_SECRET` | required | At least 32 characters; retain across migration |
| `BALANCE_ACCESS_TTL` | `15m` | Go-style duration for access tokens |
| `BALANCE_REFRESH_TTL` | `720h` | Go-style duration for refresh tokens |
| `BALANCE_ALLOW_REGISTRATION` | `true` | Enable creation of accounts |
| `BALANCE_CORS_ORIGINS` | empty | Comma-separated allowed browser origins |
| `BALANCE_ATTACHMENTS_DIR` | `attachments` beside the database | Attachment files; Docker uses `/data/attachments` inside the same volume. Back it up with the database |

Inject secrets through the deployment environment or `.env`; never put them in
images or source control. Compose preserves the existing named `balance-data`
volume mounted at `/data`. Keep that volume and its project name when upgrading.

## Go to Rust migration

1. Build and validate the Rust binary/image before stopping production. Retain the
   previous Go executable/image and exact environment for rollback.
2. Stop the Go service and confirm no other process writes its SQLite database.
3. Back up the entire database directory, including `balance.db` and any
   `balance.db-wal`/`balance.db-shm` files. Protect the backup as financial data.
   Record the existing environment securely, especially `BALANCE_JWT_SECRET`.
4. Start Rust with the **same database path/volume and JWT secret**. The schema is
   unchanged: startup creates missing tables and indexes without rewriting
   existing accounts, records, change sequences or refresh-token hashes.
5. Check `/health`, login with an existing account, refresh an existing session,
   and sync a device with its saved cursor. Verify another device receives edits
   and tombstones. Inspect error rates without logging credentials or tokens.

The existing Argon2id password hashes, HS256 JWT claims, hashed refresh tokens,
RFC3339 timestamps, entity versions and global change sequences are shared with
Go. No export/import, cursor reset, account recreation or forced sign-out is
required. Do not start both implementations against the same database at once.

## Rollback

Stop Rust first. Start the retained Go executable/image with the same current
database directory and secret. Rust-written accounts, password hashes, refresh
sessions, records, tombstones and cursors remain readable by Go. Check health,
login, refresh and sync again. Before the Go sources were removed, the
compatibility suite verified takeover and rollback in both directions; Rust still
writes the same schema (`tests/fixtures/go_schema_v1.sql`).

For database corruption or an unrelated failed migration, stop all writers before
restoring the complete backup directory. Restoring an older backup discards
changes after that backup and may require clients to resynchronize; prefer normal
binary rollback against the current database when it is healthy. Never overwrite
an active database or delete a Compose volume as part of an upgrade.

## Validation and operations

Before release run `cargo test --locked`, `cargo build --locked --release`,
and `python3 tools/compatibility.py --binary target/release/balance-server`.
The suite uses temporary databases and loopback listeners, compares
API/errors/security headers/assets with the recorded Go reference, tests browser
cookies, pagination, LWW, tombstones, concurrent pushes, token expiration, refresh
rotation, rate limiting, and SQLite persistence across restarts.

Keep regular encrypted backups with retention and restore drills. Monitor HTTP
5xx, sync errors, authentication failures, latency, database/change-log growth
and free disk space. The in-memory limiter allows 30 registration/login/refresh
attempts per remote IP per minute and resets on restart; use proxy controls when
operating a public service. Application logs must never include passwords,
Authorization headers or tokens.
