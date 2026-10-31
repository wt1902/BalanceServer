# Модель данных BalanceServer

Ниже описана серверная модель на основании исходного кода проекта. При изменении schema обновляйте этот документ.

## Где определена schema

- `src/database.rs` — создание и открытие схемы SQLite;
- `tests/fixtures/go_schema_v1.sql` — исходная схема Go-сервера v1.0.0; Rust обязан открывать такие базы без изменений;
- `src/sync.rs` — валидация и сериализация синхронизируемых записей;
- `src/auth.rs` — пользователи, пароли и refresh-сессии.

## Общие сущности

Сервер принимает сущности контракта v2 (`docs/CONTRACT_V2.md` §1):

```text
transaction category budget account transfer recurring
goal debt rate setting attachment
```

Payload хранится как непрозрачный JSON; сервер читает из него только `sha256`
записей `attachment` для сборки мусора файлов. Ниже — сущности v1.

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

## Таблицы контракта v2

Таблицы v1 (`users`, `refresh_tokens`, `records`, `changes`) не меняются. Миграция
только добавляет таблицы через `CREATE TABLE IF NOT EXISTS`, поэтому существующая
база (в том числе Go v1) открывается без переписывания строк и `sequence`.

| Таблица | Ключ | Назначение |
| --- | --- | --- |
| `ledgers` | `id` (UUID) | имя, `owner_id`, `created_at` |
| `ledger_members` | `(ledger_id, user_id)`, индекс `user_id` | `role` = `owner`/`member`, `joined_at` |
| `ledger_invites` | `code_hash` (SHA-256 кода), индекс `ledger_id` | `expires_at`, `used_at`, `used_by`; код в открытом виде не хранится |
| `ledger_records` | `(ledger_id, entity, record_id)` | текущее состояние записей пространства |
| `ledger_changes` | `sequence` AUTOINCREMENT, индекс `(ledger_id, sequence)` | поток изменений пространства |
| `attachment_blobs` | `(space, sha256)` | метаданные файлов: `content_type`, `size`, `uploaded_at`, `referenced` |

`space` в `attachment_blobs` — `user:<userId>` или `ledger:<ledgerId>`. Все таблицы
ledger ссылаются на `ledgers(id) ON DELETE CASCADE`; строки `attachment_blobs`
пространства удаляются в той же транзакции, что и ledger.

Файлы лежат в `BALANCE_ATTACHMENTS_DIR` (по умолчанию `attachments` рядом с
файлом БД): `users/<userId>/<sha[0:2]>/<sha>` и
`ledgers/<ledgerId>/<sha[0:2]>/<sha>`. Каталоги создаются с правами `0750`.
Один и тот же файл в двух пространствах хранится дважды.

## Миграции

Schema changes должны быть backward-compatible с активными версиями клиентов либо сопровождаться versioned API/миграцией.

Не переименовывайте JSON field без периода совместимости.
