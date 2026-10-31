# Архитектура BalanceServer

## Роль сервера

BalanceServer — backend для синхронизации личных финансов между устройствами.

Поток данных:

```text
iOS/macOS/watchOS ─┐
                   ├── HTTPS/JSON API ──> BalanceServer ──> Database
Android ───────────┘
```

Сервер отвечает за:
- регистрацию и авторизацию;
- пользовательские сессии;
- хранение финансовых изменений;
- push/pull synchronization;
- глобальную последовательность изменений;
- удаление через tombstones;
- выдачу изменений начиная с cursor.

## Исходный код

Production runtime — один Rust-процесс: `src/main.rs`, `runtime.rs`, `config.rs`, `database.rs`, `auth.rs`, `sync.rs`, `server.rs`, `go_json.rs`, `ledgers.rs` (общие пространства контракта v2), `attachments.rs` (файлы вложений на диске и их сборка мусора). SQLite bundled через rusqlite; HTTP обслуживается axum/hyper. `runtime.rs` сохраняет socket deadlines Go (header 5s, read 15s, write 30s, idle 60s) и ограничивает graceful shutdown десятью секундами. `go_json.rs` сохраняет особенности encoding/json и исходные payload bytes. Статические файлы из `web/` встроены при компиляции. Маршруты контракта v2 (`/v1/capabilities`, `/v1/ledgers*`, `/v1/attachments/*`) разбираются в `server.rs` до маршрутов v1, которые сохраняют поведение эталона Go.

Исходная Go-реализация удалена после v1.0.0 (исходники остались в теге `v1.0.0`). Её наблюдаемое поведение зафиксировано в `tools/compatibility_expected.txt`, а SQLite-схема — в `tests/fixtures/go_schema_v1.sql`; Rust сверяется с обоими в CI.

Конкретные границы package/handler/service/repository нужно сохранять при рефакторинге: перенос кода между слоями не должен менять внешний API.

## HTTP layer

HTTP layer принимает JSON requests от мобильных клиентов.

Каждый handler должен:
1. проверить method/content type;
2. аутентифицировать пользователя, если endpoint защищён;
3. валидировать input;
4. вызвать domain/storage logic;
5. вернуть стабильный JSON response.

## Domain / sync layer

Sync layer является наиболее критичной частью backend.

Он должен разделять:
- изменения, созданные текущим клиентом;
- изменения, которые нужно вернуть клиенту;
- удалённые записи;
- cursor/sequence.

Ключевая модель:

```text
client cursor
      │
      ▼
server change log
      │
      ├── sequence 101
      ├── sequence 102
      ├── sequence 103
      └── ...
```

Клиент может запросить изменения после своего cursor и затем сохранить новый cursor.

## Persistence

Хранилище должно сохранять как минимум:
- user/account identity;
- credentials/session metadata;
- financial records;
- change metadata;
- deletion/tombstone metadata;
- monotonically increasing sequence, если оно является частью текущего sync protocol.

Конкретные entities и schema документированы в `docs/DATA_MODEL.md`.

## Security boundaries

```text
Internet
   │
   ▼
HTTP server
   │
   ├── authentication
   ├── authorization
   ├── input validation
   │
   ▼
domain/sync
   │
   ▼
database
```

Нельзя позволять клиенту выбирать произвольный `user_id` для чужих records.

## Совместимость клиентов

Apple и Android реализации должны видеть одну и ту же семантику данных.

Особенно важно, чтобы:
- enum values были стабильными;
- даты имели единый формат;
- UUID/string IDs не менялись;
- `deleted` semantics были одинаковыми;
- cursor/sequence были монотонными;
- conflict resolution совпадал с клиентской логикой.
