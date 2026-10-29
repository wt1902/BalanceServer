# HTTP API

## Общие правила

API предназначен для iOS/macOS/watchOS и Android Balance.

Базовый URL задаётся клиентом и зависит от deployment environment.

JSON:

```http
Content-Type: application/json
```

Защищённые endpoints используют:

```http
Authorization: Bearer <access-token>
```

## Authentication

Основные операции:

```text
POST /v1/auth/register
POST /v1/auth/login
POST /v1/auth/logout
POST /v1/auth/refresh
```

### Register

Создаёт пользовательский аккаунт.

Input должен валидироваться сервером. Password никогда не хранится plaintext.

### Login

Проверяет credentials и создаёт authenticated session.

### Refresh

Принимает refresh credential и выдаёт новый access credential согласно текущей server-side session policy.

### Logout

Инвалидирует текущую session/refresh state согласно текущей реализации.

## Synchronization

Основной endpoint:

```text
POST /v1/sync
```

Логический request:

```json
{
  "cursor": 123,
  "deviceId": "UUID",
  "changes": [],
  "pull": true
}
```

Точная JSON schema должна оставаться совместимой с `ServerSync.swift` и Android `SyncRepository`.

## Change

Логическая форма:

```json
{
  "entity": "transaction",
  "id": "UUID",
  "deleted": false,
  "updatedAt": "2026-09-08T10:00:00Z",
  "payload": {}
}
```

Поддерживаемые entity:

```text
transaction
category
budget
```

## Push

Client отправляет локальные изменения.

Server должен:
1. аутентифицировать пользователя;
2. проверить ownership/ID;
3. валидировать payload;
4. применить изменения;
5. создать server change entries;
6. вернуть результат и актуальную sync metadata.

Операция должна быть безопасна при retry.

## Pull

Client передаёт cursor.

Server возвращает изменения после этого cursor, включая deletions.

Если существует pagination, ответ должен содержать понятный признак продолжения и новый cursor/sequence.

## Ошибки

Рекомендуемый контракт:

```json
{
  "error": {
    "code": "invalid_request",
    "message": "Human-readable safe message"
  }
}
```

Не возвращать:
- SQL error;
- stack trace;
- password/token;
- filesystem paths;
- внутренние identifiers инфраструктуры.

## HTTP status semantics

Рекомендуемая семантика:

```text
200 OK     — успешный request
201 Created — создание ресурса
400 Bad Request — malformed/invalid input
401 Unauthorized — missing/expired auth
403 Forbidden — resource belongs to another user
404 Not Found — resource/endpoint unavailable
409 Conflict — semantic conflict, если endpoint использует такую семантику
429 Too Many Requests — rate limit
500 Internal Server Error — internal failure
```

Конкретные status codes текущей реализации имеют приоритет над этой общей рекомендацией.
