# Разработка и запуск

## Требования

Production-сервер использует Rust 1.90+ и bundled SQLite. Go 1.25+ нужен для сохранённой эталонной реализации и differential-проверок. Python 3 используется без сторонних пакетов.

Проверить локальную версию:

```bash
rustc --version
cargo --version
go version
```

## Установка зависимостей

```bash
cargo fetch --locked
go mod download
```

## Сборка

```bash
cargo build --locked
go build ./...
```

## Тесты

```bash
cargo test --locked
cargo build --locked
python3 tools/compatibility.py
go test ./...
```

С race detector:

```bash
go test -race ./...
```

## Static checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
go vet ./...
```

Если в проекте есть Makefile/CI, его команды имеют приоритет как canonical build pipeline.

## Локальный запуск

Перед запуском изучите конфигурацию в коде и `.env`/deployment files.

Не коммитьте реальные credentials.

Собрать и запустить Rust-сервер:

```bash
cargo build --locked --release
export BALANCE_JWT_SECRET="replace-with-a-long-random-secret"
export BALANCE_DB_PATH="./data/balance.db"
./target/release/balance-server
```

По умолчанию сервер слушает `:8080`. Файл `.env` читает Docker Compose; при
прямом запуске переменные нужно передать процессу через окружение.

## Database

SQLite встроен в executable. При первом запуске сервер создаёт каталог,
таблицы и индексы автоматически. Отдельный процесс БД не нужен. При переходе
с Go сохраните существующий файл БД и `BALANCE_JWT_SECRET`; подробная
процедура и откат описаны в [DEPLOYMENT.md](DEPLOYMENT.md).

## Production

Рекомендуемый pipeline:

```text
source
  ↓
cargo fmt --check && cargo test --locked
  ↓
cargo clippy --locked --all-targets -- -D warnings
  ↓
cargo build --locked --release
  ↓
python3 tools/compatibility.py --rust-binary target/release/balance-server
  ↓
container/package
  ↓
deploy
  ↓
health check
```

Для production нужны:
- TLS termination;
- persistent database;
- backups;
- monitoring;
- log rotation;
- secrets management;
- migration procedure.

## Совместимость

При релизе server проверяйте минимум:
- старый iOS client + новый server;
- новый iOS client + server;
- Android client + server;
- concurrent sync с двух устройств;
- login/refresh после истечения access token;
- delete → pull на другом устройстве.

Production build: `cargo build --locked --release`. Прямой запуск: `BALANCE_JWT_SECRET=<secret> target/release/balance-server`, порт 8080. Docker Compose публикует порт 8123 и сохраняет прежний volume `/data`. Процедура миграции Go → Rust и rollback: [DEPLOYMENT.md](DEPLOYMENT.md).
