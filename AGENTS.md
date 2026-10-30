# AGENTS.md

## Назначение

**BalanceServer** — серверная часть приложения Balance, используемая iOS/macOS/watchOS и Android-клиентами.

Этот файл является operational guide для AI-агентов и разработчиков. При изменениях сначала изучай существующий код и сохраняй текущий API-контракт клиентов.

## Быстрая карта проекта

Сервер написан на Rust 1.90+. Исходная Go-реализация удалена после v1.0.0
(её исходники остались в теге `v1.0.0`), но её поведение остаётся эталоном API:
ответы Go записаны в `tools/compatibility_expected.txt`, а исходная SQLite-схема
Go — в `tests/fixtures/go_schema_v1.sql`. Оба файла не правь вручную.

Основные Rust-модули: `src/main.rs`, `runtime.rs`, `config.rs`,
`database.rs`, `auth.rs`, `server.rs`, `sync.rs`, `go_json.rs`.
Веб-клиент из `web/` встроен в Rust executable.

Обнаруженные технологии/интеграции: SQLite, Docker.

## Правила изменений

### 1. Не ломать клиентский контракт

Сервер обслуживает несколько клиентов:
- Apple Balance;
- Android Balance.

Изменение JSON-полей, HTTP methods, endpoint paths, auth semantics или sync semantics считается breaking change, даже если сервер компилируется.

Перед изменением API ищи использование endpoint/field в server code и документации клиентов.

### 2. Sync — центральный контракт

Синхронизация должна оставаться:
- детерминированной;
- идемпотентной при повторной доставке;
- устойчивой к повторным запросам;
- корректной при нескольких устройствах одного пользователя.

Не меняй правила cursor/sequence/update timestamps без обновления `docs/SYNC.md`.

### 3. Аутентификация

Никогда не:
- логируй пароль;
- логируй access/refresh token;
- возвращай секреты в ошибках;
- храни plaintext passwords.

При изменении auth обязательно проверь:
- регистрацию;
- login;
- refresh;
- logout;
- истечение токенов;
- авторизацию sync endpoint.

### 4. База данных

Любое изменение schema должно иметь:
- migration strategy;
- обратную совместимость там, где её требуют старые клиенты;
- проверку индексов для user/entity/id/sequence/update timestamp.

Не удаляй существующие поля только потому, что текущий клиент ими не пользуется.

### 5. Конкурентность

Сервер должен быть безопасен при одновременных запросах от нескольких устройств.

Особое внимание:
- генерации sequence;
- выдаче cursor;
- записи изменений;
- refresh-token rotation;
- race между push и pull.

### 6. Ошибки

HTTP API должен возвращать стабильную структуру ошибок. Не отдавай stack traces, SQL errors или внутренние пути клиенту.

Внутренние детали логируй только в безопасном виде.

### 7. Тесты

Перед PR:

```bash
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
python3 tools/compatibility.py --binary target/release/balance-server
```

Если `tools/compatibility.py` нашёл расхождение, это изменение видно клиентам.
Перезаписывай эталон (`--update`) только для намеренного изменения API и
после обновления документации.

Если проект использует дополнительные проверки/линтеры, запускай их по Makefile/CI.

### 8. Документация

При изменении:
- endpoint → обновить `docs/API.md`;
- sync → `docs/SYNC.md`;
- DB model → `docs/DATA_MODEL.md`;
- deploy/config → `docs/DEPLOYMENT.md`;
- архитектуры → `docs/ARCHITECTURE.md`.

## Checklist

- [ ] Не изменён API случайно.
- [ ] Auth secrets не попадают в logs/errors.
- [ ] Sync остаётся идемпотентным.
- [ ] DB migration учтена.
- [ ] Concurrent requests безопасны.
- [ ] Rust tests, fmt, clippy и release build проходят.
- [ ] `tools/compatibility.py` совпадает с записанным эталоном Go.
- [ ] Документация обновлена.
