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

Поддерживаемые entity (контракт v2, `docs/CONTRACT_V2.md` §1):

```text
transaction category budget account transfer recurring
goal debt rate setting attachment
```

Незнакомая entity отклоняется `400 invalid_change`; текст сообщения
(`entity must be transaction, category, or budget`) сохранён из v1, потому что
он записан в эталоне `tools/compatibility_expected.txt`. Payload сервер хранит
как есть и не валидирует по сущности.

Заголовок `X-Balance-Ledger: <id>` выбирает общее пространство (см. ниже). Без
заголовка (или с пустым значением) — личное пространство, как в v1.

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

## Контракт v2

### Capabilities

`GET /v1/capabilities` — без авторизации:

```json
{"contract":2,
 "entities":["transaction","category","budget","account","transfer","recurring","goal","debt","rate","setting","attachment"],
 "attachments":{"maxBytes":10485760,"types":["image/jpeg","image/png","image/heic","application/pdf"]},
 "ledgers":true}
```

### Общие пространства (ledgers)

Все запросы — с `Authorization`. Объект ledger:
`{"id","name","role","members":[{"userId","email","role"}]}`, `role` —
роль текущего пользователя (`owner` | `member`), владелец идёт первым в `members`.

| Метод | Путь | Успех | Кто |
| --- | --- | --- | --- |
| GET | `/v1/ledgers` | `200 {"ledgers":[…]}` (только общие) | любой |
| POST | `/v1/ledgers` `{"name"}` | `201` ledger, `role:"owner"` | любой |
| PATCH | `/v1/ledgers/{id}` `{"name"}` | `200` ledger | владелец |
| DELETE | `/v1/ledgers/{id}` | `204` | владелец; удаляет записи, курсор, приглашения, участников и файлы вложений |
| POST | `/v1/ledgers/{id}/invites` | `201 {"code","expiresAt"}` | владелец |
| POST | `/v1/ledgers/join` `{"code"}` | `200` ledger, `role:"member"` | любой |
| DELETE | `/v1/ledgers/{id}/members/{userId}` | `204` | владелец — любого участника; участник — себя |

- Имя: после `trim` 1–100 символов без управляющих → иначе `400 invalid_name`.
- Код приглашения: 10 символов из `ABCDEFGHJKLMNPQRSTUVWXYZ23456789`, живёт 7 дней,
  одноразовый. При вводе регистр, пробелы и `-` игнорируются. Повторный join
  участника возвращает ledger и код не расходует. `/v1/ledgers/join` входит в
  общий лимит 30 запросов/мин на IP вместе с auth endpoints.
- Ошибки: `400 invalid_code` (неверный, использованный или просроченный код),
  `403 forbidden` (не участник или не владелец), `404 not_found` (нет такого
  пространства или участника), `409 owner_cannot_leave` (владелец пытается
  выйти — он может только удалить пространство), `400 invalid_json`.
- `X-Balance-Ledger` на `/v1/sync` и `/v1/attachments/*`: не участник → `403
  forbidden`, несуществующее → `404 not_found`.

### Вложения

| Метод | Путь | Ответ |
| --- | --- | --- |
| PUT | `/v1/attachments/{sha256}` | тело — байты; `201` первый раз, `200` повтор; `{"sha256","size","contentType"}` |
| GET/HEAD | `/v1/attachments/{sha256}` | `200`, байты, сохранённый `Content-Type`, `Content-Disposition: attachment`; `404 not_found`, если в пространстве нет файла |

- `{sha256}` — 64 hex-символа в нижнем регистре; SHA-256 тела должен совпасть,
  иначе `400 hash_mismatch` (так же для неверного формата хеша в PUT).
- `Content-Type` (параметры игнорируются) — `image/jpeg`, `image/png`,
  `image/heic`, `application/pdf`, иначе `415 unsupported_media_type`.
- Больше 10 MiB → `413 payload_too_large`. Лимит тела 10 MiB действует только для
  авторизованного PUT; остальные запросы по-прежнему ограничены 2 MiB.

### CORS

Ответы v1 сохраняют заголовки эталона Go. Для маршрутов v2 и для настоящих
preflight-запросов (есть `Access-Control-Request-Method`) разрешённые origin
получают `Access-Control-Allow-Methods: GET, POST, PUT, PATCH, DELETE, OPTIONS`
и `Access-Control-Allow-Headers: Authorization, Content-Type, X-Balance-Ledger`.

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
