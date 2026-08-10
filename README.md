# DB Extractor CLI

Утилита для быстрой выгрузки данных из реляционных баз данных (включая Oracle, PostgreSQL, MySQL и др.) в форматы Parquet и CSV. Построена на базе `connectorx` (Rust) и `polars` (Apache Arrow).

## Синтаксис

```bash
db-extractor --conn <CONNECTION_STRING> [ОПЦИИ]
```

### Обязательные параметры:
- `--conn` : Строка подключения к базе данных (например, `oracle://user:pass@host:port/service` или `postgresql://user:pass@host:port/dbname`).
- **ОДИН** из двух параметров для SQL-запроса:
  - `--query` : SQL-запрос текстом прямо в консоли.
  - `--query-file` : Путь к текстовому файлу `.sql`, содержащему запрос(ы).

### Опциональные параметры:
- `--out` : Путь к итоговому файлу (например, `data.parquet`). Обязателен, если не указан внутри SQL-запросов через спец-комментарии.
- `--format` : Формат вывода (`csv` или `parquet`). По умолчанию определяется по расширению файла.
- `--partition-on` : Имя числовой колонки (обычно ID) для параллельной выгрузки (нарезки).
- `--partitions` : Количество потоков/партиций (работает вместе с `--partition-on`).

---

## Настройки в SQL-комментариях
Чтобы гибко управлять каждым запросом при пакетной выгрузке, можно использовать специальные аннотации в SQL-комментариях:

```sql
-- @DX_OUT: reports_2023.parquet
-- @DX_PARTITION_ON: report_id
-- @DX_PARTITIONS: 4
SELECT * FROM reports WHERE year = 2023;

-- @DX_OUT: users_active.csv
SELECT * FROM users WHERE active = true;
```
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

**4. Многопоточная выгрузка через CLI:**
```bash
db-extractor --conn "postgresql://user:pass@localhost:5432/mydb" \
             --query "SELECT * FROM large_events" \
             --partition-on "event_id" \
             --partitions 10 \
             --out "events.parquet"
```
