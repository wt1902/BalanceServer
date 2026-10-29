# Balance Server

Self-hosted synchronization, account and web server for the Balance iOS, macOS, watchOS and Android applications. It is a single Rust process with a bundled SQLite engine and a complete browser client, and runs on Linux, Windows and macOS.

## Quick start with Docker

```sh
cp .env.example .env
openssl rand -base64 48
# Paste the generated value into BALANCE_JWT_SECRET in .env
docker compose up -d --build
curl http://localhost:8123/health
```

Open `http://localhost:8123` to register or sign in to the web application. Data is kept in the `balance-data` Docker volume. Back it up while the container is stopped.

## Run without Docker

Install Rust 1.90 or newer, then build the locked dependencies:

```sh
cargo build --locked --release
```

The binary is `target/release/balance-server` (`balance-server.exe` on Windows).
SQLite is compiled into the binary; no separate SQLite service is needed.

On Linux or macOS:

```sh
export BALANCE_JWT_SECRET="replace-with-a-long-random-secret"
export BALANCE_DB_PATH="./data/balance.db"
./target/release/balance-server
```

On Windows PowerShell:

```powershell
$env:BALANCE_JWT_SECRET = "replace-with-a-long-random-secret"
$env:BALANCE_DB_PATH = ".\data\balance.db"
.\target\release\balance-server.exe
```

For another platform, build on that platform or install its Rust target and C toolchain
for bundled SQLite. `cargo build --locked --release --target <target>` requires the
matching linker and C compiler. The retained Go implementation is a compatibility
reference and rollback option: `go build -o balance-server-go ./cmd/balance-server`.

After starting the binary, open `http://localhost:8080` in a browser. The web assets are embedded into the executable, so there is no separate frontend build, Node.js process or static directory to deploy.

## Web application

The responsive web client includes:

- registration, login, automatic session refresh and logout;
- dashboard, current balance and manual balance adjustment;
- adding, editing, deleting, searching and filtering transactions;
- analytics for 1, 3 and 12 months, spending breakdown and savings rate;
- monthly category budgets;
- custom categories with 50 icons, custom emoji and 24 colors;
- light, dark and system themes, plus RUB, EUR, USD and SEK formatting;
- automatic synchronization every 30 seconds and whenever the browser tab becomes active;
- installable PWA shell for desktop and mobile browsers.

The browser session uses `HttpOnly`, `SameSite=Strict` cookies. Native applications continue to use Bearer access tokens and are fully backward compatible. Only display preferences are saved in browser local storage; tokens and financial records are not stored there.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `BALANCE_ADDR` | `:8080` | Listen address |
| `BALANCE_DB_PATH` | `./data/balance.db` | SQLite file path |
| `BALANCE_JWT_SECRET` | required | Random secret of at least 32 characters |
| `BALANCE_ALLOW_REGISTRATION` | `true` | Allows new accounts |
| `BALANCE_ACCESS_TTL` | `15m` | Access-token lifetime |
| `BALANCE_REFRESH_TTL` | `720h` | Refresh-token lifetime |
| `BALANCE_CORS_ORIGINS` | empty | Comma-separated browser origins; native apps do not need CORS |

## HTTPS

Use HTTPS before exposing the server to a network. The Apple applications accept plain HTTP only for `localhost` and `127.0.0.1`; a physical device should connect through an HTTPS reverse proxy such as Caddy, nginx or a private VPN with TLS. Example Caddy configuration:

```caddyfile
balance.example.com {
    reverse_proxy 127.0.0.1:8123
}
```

Then enter `https://balance.example.com` in each application's Settings → Server account.

## API

| Method | Endpoint | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Health check |
| `POST` | `/v1/auth/register` | Create account |
| `POST` | `/v1/auth/login` | Sign in |
| `POST` | `/v1/auth/refresh` | Rotate session tokens |
| `POST` | `/v1/auth/logout` | Revoke refresh token |
| `GET` | `/v1/me` | Current account |
| `POST` | `/v1/sync` | Push and pull changes |

The sync endpoint accepts transactions, custom categories and monthly budgets. The server isolates records by account, stores an append-only change cursor, and resolves competing edits with last-write-wins timestamps. Current Apple and Android clients use push-only requests followed by paginated pulls; update the server and applications together.

## Operational notes

- Registration can be disabled after the required accounts have been created.
- Passwords are stored as salted Argon2id hashes. Refresh tokens are stored only as SHA-256 hashes.
- Back up the SQLite database and its `-wal`/`-shm` files together, or stop the server before copying the database.
- Changing `BALANCE_JWT_SECRET` invalidates access tokens. Existing refresh sessions remain valid and will receive new access tokens after refresh.
- The in-process authentication limiter allows 30 authentication requests per IP per minute. Use a reverse proxy for stronger public rate limiting and request logging.

## Compatibility and migration

The Rust server retains the HTTP/JSON API, embedded web client, environment
variables, SQLite schema, Argon2id password hashes, JWT format, refresh sessions
and sync cursor semantics of the Go implementation. Docker Compose still maps
host port **8123** to container port **8080** and keeps `/data` in `balance-data`.
Direct binary execution defaults to port 8080.

Before replacing a running Go server, stop it, back up its database directory
(including any SQLite WAL/SHM files), and preserve `BALANCE_JWT_SECRET`. Start Rust
against the same `BALANCE_DB_PATH`; do not run both writers simultaneously.
Existing accounts, access/refresh tokens and saved device cursors remain usable.
See [deployment and rollback](docs/DEPLOYMENT.md) for the full procedure.

Verify the implementations locally with Go 1.25+, Rust and Python 3:

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
python3 tools/compatibility.py
go test ./...
go vet ./...
go build ./...
```

The Python standard-library suite builds the Go reference, starts both binaries
on temporary loopback ports and databases, compares API responses and assets,
and tests SQLite takeover and rollback in both directions. To use release or
prebuilt executables, pass `--rust-binary PATH` and optionally `--go-binary PATH`.
