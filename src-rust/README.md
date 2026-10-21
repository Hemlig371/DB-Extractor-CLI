# DB Extractor, сборка на Rust

Интерфейс совпадает с Python-версией (`src-python`): флаги, аннотации `-- @DX_*`, формат `export_log.txt`, коды возврата `0/1/2`. Описание всех параметров: [основной README](../README.md).

## Сравнение с Python-версией

| | Python | Rust |
|---|---|---|
| Результат (Parquet, Arrow IPC, CSV, JSONL, хеши) | совпадает с Rust: имена колонок, типы и значения; CSV и JSONL побайтно | совпадает с Python |
| Память | результат запроса целиком в памяти | данные идут пакетами (`--batch-size`, по умолчанию 100 000 строк) и сразу пишутся на диск |
| Параметр `--batch-size` | нет | есть |
| Запуск | PyInstaller-пакет, папка с библиотеками | один исполняемый файл |
| Источники | polars и connectorx из pip | connectorx 0.4.6, все источники собраны в бинарник, выбор по схеме URI во время работы |
| Типы Oracle | `BOOLEAN`, `RAW`, `LONG RAW`, `NCLOB`, `INTERVAL`, `ROWID`, `TIMESTAMP WITH [LOCAL] TIME ZONE` нужно приводить в SQL | эти типы читаются |
| SQL Server на Linux | логин и пароль, интегрированная аутентификация (Kerberos) | только логин и пароль |
| Порядок строк при партиционировании | не гарантируется | не гарантируется |
| Сообщение `Extracted and processed N rows` | до записи файла | после записи всех строк, перед финализацией файла |

## Источники данных

Oracle, PostgreSQL, ClickHouse, MySQL/MariaDB, SQL Server/Azure SQL, SQLite, BigQuery, Trino, Redshift. Подключение только через URI (таблица с примерами в основном README). Для другой БД из этого списка пересобирать бинарник не нужно. Для базы, которой в списке нет, используйте DuckDB.

Для Oracle на машине должен стоять Oracle Instant Client той же разрядности, что и `db-extractor`; его каталог добавляется в `PATH` (Windows) или `LD_LIBRARY_PATH` (Linux). Бинарник запускается и без клиента: ошибка появляется только при подключении к Oracle, с подсказкой.

Вход в SQL Server на Linux только по логину и паролю: интегрированная аутентификация (Kerberos) не включена, поэтому бинарник не зависит от системных библиотек Kerberos. На Windows интегрированная аутентификация работает.

## Потоковая запись

Схема результата известна до чтения строк, поэтому все преобразования (`--hash-columns`, `--utc`, `--fill-nulls`, `--drop-nulls`, `--decimal-as`, `--cast-universal`, `--categorize`) строятся по схеме один раз и применяются к каждому пакету. Типы после `--cast-universal` не зависят от данных: каждый пакет получает одинаковые типы.

Пакетом называется `--batch-size` строк. Parquet дополнительно накапливает одну группу строк (262 144) перед записью. connectorx передаёт пакеты в записывающий код по очереди без верхней границы, поэтому жёсткого предела памяти нет: обычно чтение из БД медленнее записи и очередь остаётся маленькой, иначе память растёт вплоть до размера результата.

## Одинаковый результат

CSV и JSONL Rust-сборка пишет собственным кодом (`db-extractor/src/textfmt.rs`), который повторяет форматирование polars: числа с плавающей точкой, даты, время, decimal, бинарные данные, экранирование. Хеши для `--hash-columns` считаются от того же текста, что даёт приведение к строке в polars. Формат значений описан в [основном README](../README.md#формат-значений-в-csv-и-jsonl).

## Обработка ошибок и неподдерживаемые типы

connectorx сообщает о неподдерживаемых типах паникой (`unimplemented!`), а не обычной ошибкой. Бинарник перехватывает такие паники и превращает их в запись `ERROR` в журнале и сообщение с подсказкой: приведите колонку в самом SQL (`TO_CHAR(col)`, `CAST(col AS VARCHAR)`). Сюда относятся, например, PostgreSQL `interval`, `money`, `xml` и `numeric` без указания точности.

Типы Oracle, на которых оригинальный connectorx падает или возвращает неверный результат, в этой сборке читаются. Список изменений: [vendor/connectorx/VENDORED.md](vendor/connectorx/VENDORED.md).

## Сборка

```bash
cd src-rust
cargo build --release -p db-extractor
cargo test --workspace --release
```

Результат: `target/release/db-extractor`. Нужны Rust (stable); на Linux ещё `pkg-config` и `libssl-dev`; для сборки OpenSSL на Windows нужен Perl (есть в Git for Windows и Strawberry Perl). Первая сборка долгая, следующие используют кэш.

Структура:
- `vendor/connectorx` : копия connectorx 0.4.6 с изменениями для Oracle и без Kerberos.
- `dbx-core` : логика без внешних зависимостей: разбор SQL, аннотации, выбор формата и сжатия, сопоставление колонок, маскирование паролей, формат журнала, текст значений для CSV и JSONL. Тесты: `cargo test -p dbx-core`.
- `db-extractor` : чтение (`extract`), преобразования (`transform`), запись (`writers`, `textfmt`), запуск и журнал (`run`), командная строка (`cli`).

## Сравнение результатов двух версий

`tests/parity/run_parity.py` запускает обе версии на одной SQLite-базе (форматы, сжатие, хеши, заполнение и удаление NULL, UTC, универсальный каст, категоризация, партиции, пустой результат, ошибки, аннотации) и сравнивает коды возврата, записи журнала и содержимое файлов. CSV и JSONL сравниваются побайтно, Parquet и Arrow по именам колонок, семействам типов и значениям.

```bash
pip install -r requirements.txt
python tests/parity/run_parity.py --rust src-rust/target/release/db-extractor
```

Типы конкретных СУБД проверяет workflow `databases.yml` (запуск вручную): те же сценарии на PostgreSQL 16, Oracle Free 23 и ClickHouse, таблицы создают `tests/parity/postgres.sql`, `tests/parity/oracle.sql` и `tests/parity/clickhouse.sql`. Для ClickHouse дополнительно выгруженный Parquet (`--decimal-as float`, `string`, `decimal`) загружается обратно в ClickHouse. Типы Oracle, которые читает только Rust-сборка, проверяются по ожидаемому содержимому.
