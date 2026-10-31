# Безопасность

## Credentials

Пароли пользователей должны храниться только в виде slow password hash с современным password hashing алгоритмом.

Не хранить plaintext password.

## Tokens

Access/refresh tokens не должны попадать в:
- application logs;
- panic messages;
- error responses;
- analytics;
- database debug dumps.

## Authorization

Каждый sync mutation должен проверять ownership по authenticated user.

Нельзя доверять `userId` из client payload, если authenticated identity уже известна из token.

Правильная модель:

```text
Authorization header
        ↓
authenticated user
        ↓
server-side ownership
        ↓
requested entity
```

## Transport

Production API должен работать через HTTPS/TLS.

HTTP следует оставлять только для controlled local development.

## Input validation

Проверяйте:
- UUID/ID;
- enum values;
- amount;
- timestamps;
- string lengths;
- batch size;
- JSON size;
- cursor/sequence bounds.

## Общие пространства и вложения (контракт v2)

- Доступ к ledger проверяется на каждый запрос по `ledger_members`; `X-Balance-Ledger`
  не участника → `403`, неизвестный → `404`. Управлять пространством и выдавать
  приглашения может только владелец.
- Коды приглашений — 10 случайных символов из 32 (50 бит, `OsRng`), одноразовые,
  7 дней; в БД хранится только SHA-256. `/v1/ledgers/join` ограничен общим
  лимитом 30 запросов/мин на IP.
- Вложения проверяются до записи на диск: формат хеша, тип из белого списка,
  размер ≤ 10 MiB и SHA-256 тела. Файл пишется во временный файл и
  переименовывается. Тело больше 2 MiB читается только после проверки
  access token. Путь к файлу строится из проверенного hex-хеша и id
  пространства (небезопасный id заменяется его хешем), поэтому значения клиента
  не становятся компонентами пути.
- Файлы отдаются с `Content-Disposition: attachment`, `X-Content-Type-Options:
  nosniff`, `Cache-Control: no-store` и не отображаются в origin веб-клиента.
- Каталог вложений содержит финансовые документы: права `0750`, резервные
  копии — вместе с БД.

## Logging

Безопасно логировать:
- request ID;
- endpoint;
- status code;
- latency;
- batch size;
- non-sensitive user/record identifiers в допустимой для проекта форме.

Нельзя логировать:
- passwords;
- Authorization headers;
- access tokens;
- refresh tokens;
- полные request bodies финансовых данных без явной необходимости.

## Abuse protection

Production deployment должен иметь:
- rate limiting для auth endpoints;
- reasonable request body limits;
- timeouts;
- graceful shutdown;
- database connection limits;
- monitoring.
