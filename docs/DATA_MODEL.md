# Модель данных BalanceServer

Ниже описана серверная модель на основании исходного кода проекта. При изменении schema обновляйте этот документ.

## Найденные определения моделей/schema

- internal/database/database.go
- internal/database/database.go
- internal/database/database.go
- internal/config/config.go
- internal/auth/tokens.go
- internal/auth/tokens.go
- internal/httpapi/auth_handlers.go
- internal/httpapi/auth_handlers.go
- internal/httpapi/auth_handlers.go
- internal/httpapi/server.go
- internal/httpapi/server.go
- internal/httpapi/server.go
- internal/httpapi/sync_handler.go
- internal/httpapi/sync_handler.go
- internal/httpapi/sync_handler.go
- internal/httpapi/web.go
- internal/httpapi/server_test.go
- internal/httpapi/server_test.go
- internal/httpapi/server_test.go

## Общие сущности

Сервер должен различать как минимум следующие логические типы синхронизируемых данных, совместимые с клиентами Balance:

```text
transaction
category
budget
```

### Transaction

Логически содержит:
- стабильный ID;
- amount;
- date;
- note;
- category presentation data;
- transaction kind;
- balance-adjustment flag;
- update timestamp;
- принадлежность пользователю.

### Category

Логически содержит:
- стабильный ID;
- name;
- icon/emoji;
- color;
- operation kind;
- creation/update metadata;
- принадлежность пользователю.

### Budget

Логически содержит:
- стабильный ID;
- category information;
- monthly limit;
- month start;
- update timestamp;
- принадлежность пользователю.

## Change metadata

Для sync необходима метаинформация:

```text
user
entity
record ID
deleted
updatedAt
sequence
payload
```

`sequence` используется как серверный порядок изменений, а `updatedAt` — как timestamp версии самой записи.

## Tombstones

Удаление нельзя реализовывать только физическим `DELETE`, если клиентам нужно узнать о нём при следующем pull.

Используется логическое изменение:

```json
{
  "entity": "transaction",
  "id": "…",
  "deleted": true,
  "updatedAt": "…"
}
```

После того как tombstone перестанет быть нужен для поддерживаемого sync history window, сервер может применять retention policy. Такая политика должна быть согласована с cursor semantics.

## Invariants

1. Record ID стабилен между sync.
2. Record принадлежит одному пользователю.
3. Клиент не может читать/изменять чужую запись.
4. `sequence` не должен уменьшаться.
5. Повторная доставка одного change не должна повреждать данные.
6. Удаление должно быть видимо клиенту через sync.
7. Timestamps должны храниться в UTC.
8. Enum/string values должны быть совместимы с iOS и Android.

## Миграции

Schema changes должны быть backward-compatible с активными версиями клиентов либо сопровождаться versioned API/миграцией.

Не переименовывайте JSON field без периода совместимости.
