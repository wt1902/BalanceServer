# Синхронизация

## Цель

Один аккаунт Balance может использоваться на нескольких устройствах:

```text
iPhone
   │
   ├── transaction A
   ├── category B
   └── budget C
        │
        ▼
   BalanceServer
        │
   ┌────┴────┐
   ▼         ▼
 Android    Mac
```

## Cursor

Каждый клиент хранит cursor последнего успешно применённого server change.

Принцип:

```text
cursor = 100

server:
101 A
102 B
103 C

pull(cursor=100)
→ A, B, C
→ new cursor = 103
```

Клиент должен сохранять cursor только после успешного применения соответствующего batch.

## Sequence

Server sequence задаёт глобальный порядок изменений в sync stream.

Требования:
- монотонность;
- отсутствие уменьшения;
- однозначность порядка;
- корректность при concurrent requests.

## Push

Push передаёт изменения клиента:

```text
local mutations
      ↓
POST /v1/sync
      ↓
validation
      ↓
ownership check
      ↓
persist
      ↓
append to change stream
```

Retry не должен создавать две разные логические записи из одного client change.

## Pull

Pull получает изменения после cursor.

Сервер не должен возвращать изменения другого пользователя.

При pagination клиент должен иметь возможность продолжить с нового cursor.

## Conflict resolution

Клиенты Balance используют timestamp-based versioning.

Типовая проверка:

```text
local.updatedAt < remote.updatedAt
```

Следовательно, server должен сохранять `updatedAt` достаточно точно и не подменять его произвольным временем каждого чтения.

При конкурентных writes сервер должен иметь однозначное правило победителя.

## Deletions

Удаление создаёт tombstone/change:

```json
{
  "entity": "transaction",
  "id": "...",
  "deleted": true,
  "updatedAt": "..."
}
```

Tombstone должен попасть в change stream так же, как обычное изменение.

## Idempotency

Push может повториться из-за:
- timeout;
- network failure;
- app restart;
- retry after 401/token refresh.

Поэтому сервер должен обрабатывать повторную отправку безопасно.

## Batch

Apple-клиент отправляет batches до 100 изменений и может делить крупные payloads.

Android должен использовать совместимый protocol.

Server не должен предполагать, что все изменения приходят одним запросом.

## Reset/full sync

Если client потерял cursor или требует reset, сервер должен поддерживать предусмотренный текущим API механизм полного pull либо новый cursor baseline.

Нельзя молча трактовать неизвестный/просроченный cursor как актуальный.

## Security

Sync endpoint всегда ограничен текущим authenticated user.

`deviceId` — идентификатор устройства, а не механизм авторизации.
