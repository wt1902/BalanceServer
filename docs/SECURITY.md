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
