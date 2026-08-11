# DB Extractor CLI

Утилита для быстрой выгрузки данных из реляционных баз данных (включая Oracle, PostgreSQL, MySQL и др.) в форматы Parquet и CSV. Построена на базе `connectorx` (Rust) и `polars` (Apache Arrow).

## Синтаксис

```bash
db-extractor --conn <CONNECTION_STRING> [ОПЦИИ]
```

### Обязательные параметры:
- **ОДИН** из двух параметров для подключения:
  - `--conn` : Строка подключения к базе данных (например, `oracle://user:pass@host:port/service` или `postgresql://user:pass@host:port/dbname`).
  - `--conn-file` : Путь к текстовому файлу, содержащему строку подключения.
- **ОДИН** из двух параметров для SQL-запроса:
  - `--query` : SQL-запрос текстом прямо в консоли.
  - `--query-file` : Путь к файлу `.sql` с запросами, ИЛИ путь к папке (в таком случае обработаются все `.sql` файлы внутри).

### Опциональные параметры:
- `--out` : Путь к итоговому файлу (например, `data.parquet`). Обязателен, если не указан внутри SQL-запросов через спец-комментарии.
- `--format` : Формат вывода (`csv`, `parquet`, `arrow`, `jsonl`). По умолчанию определяется по расширению файла.
- `--compression` : Алгоритм сжатия и (опционально) уровень сжатия через двоеточие. Примеры: `zstd`, `zstd:10`, `gzip:6`. Для Parquet по умолчанию `zstd`, для CSV — без сжатия.
- `--partition-on` : Имя числовой колонки (обычно ID) для параллельной выгрузки (ConnectorX).
- `--partitions` : Количество потоков/партиций (работает вместе с `--partition-on`).

### Дополнительные возможности:
- `--continue-on-error` : Продолжить выполнение остальных запросов при ошибке в одном из них. При этом утилита всё равно вернёт код ошибки (`exit 1`) в самом конце, если были упавшие запросы.
- `--workers` : Количество одновременных потоков/воркеров для скачивания нескольких таблиц (Thread Pool). По умолчанию `1`.
- `-q`, `--quiet` : Тихий режим. Отключает вывод в консоль (логи об ошибках по-прежнему пишутся в `export_log.txt`).
- `--fill-nulls` : Заполнить пустоты (null) дефолтными значениями (`'null'` для строк, `0` для чисел). Можно указать конкретные колонки через пробел, либо оставить пустым для всех.
- `--drop-nulls` : Удалить строки с пропусками. Можно указать конкретные колонки через пробел.
- `--utc` : Конвертировать все столбцы с датой/временем в UTC.
- `--categorize` : Список строковых столбцов для перевода в категориальный тип (Enum/LowCardinality).
- `--hash-columns` : Список столбцов для хэширования (маскирование PII, например, email).

---

## Продвинутые возможности

### 1. Выполнение нескольких запросов (разделение по `;`)
Если вы используете `--query-file`, можно указать несколько независимых SQL-запросов, разделив их точкой с запятой (`;`). Утилита выполнит их по очереди, очищая память между запусками.

### 2. Настройки прямо в SQL-комментариях
Чтобы гибко управлять каждым запросом при пакетной выгрузке, можно использовать специальные аннотации в SQL-комментариях:

```sql
-- @DX_OUT: reports_2023.parquet
-- @DX_PARTITION_ON: report_id
-- @DX_PARTITIONS: 4
-- @DX_COMPRESSION: zstd:10
-- @DX_CATEGORIZE: status, type
-- @DX_FILL_NULLS: status, category
SELECT * FROM reports WHERE year = 2023;

-- @DX_OUT: users_active.csv
-- @DX_COMPRESSION: gzip:6
-- @DX_HASH_COLUMNS: email, phone
-- @DX_UTC
SELECT * FROM users WHERE active = true;
```
Доступные аннотации: `@DX_OUT`, `@DX_FORMAT`, `@DX_COMPRESSION`, `@DX_PARTITION_ON`, `@DX_PARTITIONS`, `@DX_FILL_NULLS`, `@DX_DROP_NULLS`, `@DX_UTC`, `@DX_CATEGORIZE`, `@DX_HASH_COLUMNS`.
Параметры в комментариях переопределяют глобальные параметры (переданные через CLI).

### 3. Автоматическое логирование
Утилита автоматически ведет журнал работы в файле `export_log.txt` (в папке запуска). В лог записываются: время старта, длительность, количество выгруженных строк, итоговый размер файла в МБ, текст запроса и статус (SUCCESS/ERROR).

---

## Примеры использования

**1. Простая выгрузка в Parquet в один поток (запрос текстом):**
```bash
db-extractor --conn "postgresql://user:pass@localhost:5432/mydb" \
             --query "SELECT * FROM public.users WHERE active = true" \
             --out "users.parquet"
```

**2. Выгрузка в CSV с использованием SQL-файла (классический способ):**
```bash
db-extractor --conn "oracle://user:pass@10.0.0.1:1521/ORCL" \
             --query-file "./scripts/complex_report.sql" \
             --out "report.csv"
```

**3. Пакетная выгрузка нескольких таблиц:**
```bash
db-extractor --conn "oracle://user:pass@10.0.0.1:1521/ORCL" \
             --query-file "./scripts/multi_export.sql"
```

**4. Многопоточная выгрузка через CLI (и сжатие ZSTD):**
```bash
db-extractor --conn "postgresql://user:pass@localhost:5432/mydb" \
             --query "SELECT * FROM large_events" \
             --partition-on "event_id" \
             --partitions 10 \
             --compression zstd \
             --out "events.parquet"
```
