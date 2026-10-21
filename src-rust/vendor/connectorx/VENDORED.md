# connectorx (локальная копия)

Исходник: https://github.com/sfu-db/connector-x, тег v0.4.6, каталог `connectorx/` (лицензия MIT, файл LICENSE).

Изменения относительно оригинала:

1. `Cargo.toml`: зависимости из workspace записаны явно; убраны `dev-dependencies` и `readme`; у `tiberius` убран `integrated-auth-gssapi` (бинарник не зависит от библиотек Kerberos, вход в SQL Server на Linux только по логину и паролю; на Windows интегрированная аутентификация работает).
2. Oracle (`src/sources/oracle/typesystem.rs`, `src/sources/oracle/mod.rs`, `src/transports/oracle_arrow.rs`, `src/transports/oracle_arrowstream.rs`): добавлено чтение типов, на которых оригинал падал с `unimplemented!`.

| Тип Oracle | Результат |
|---|---|
| `BOOLEAN` | Boolean |
| `RAW`, `LONG RAW` | бинарные данные |
| `NCLOB` | строка |
| `INTERVAL DAY TO SECOND`, `INTERVAL YEAR TO MONTH` | строка в формате Oracle, например `+01 02:03:04.000000` |
| `ROWID` | строка |
| `TIMESTAMP WITH LOCAL TIME ZONE` | момент времени в UTC (как `TIMESTAMP WITH TIME ZONE`) |

При обновлении connectorx эти изменения нужно перенести в новую версию.
