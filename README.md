# DB Extractor CLI

Утилита для быстрой выгрузки данных из реляционных баз данных (включая Oracle, PostgreSQL, MySQL и др.) в форматы Parquet и CSV. Построена на базе `connectorx` (Rust) и `polars` (Apache Arrow).

## Синтаксис

```bash
db-extractor --conn <CONNECTION_STRING> [ОПЦИИ] --out <OUTPUT_FILE>
```

### Обязательные параметры:
- `--conn` : Строка подключения к базе данных (например, `oracle://user:pass@host:port/service` или `postgresql://user:pass@host:port/dbname`).
- `--out` : Путь к итоговому файлу (например, `data.parquet` или `export.csv`).
- **ОДИН** из двух параметров для SQL-запроса:
  - `--query` : SQL-запрос текстом прямо в консоли.
  - `--query-file` : Путь к текстовому файлу `.sql`, содержащему запрос.

### Опциональные параметры:
- `--format` : Формат вывода (`csv` или `parquet`). Если не указан, утилита попытается определить формат автоматически по расширению файла `--out`.
- `--partition-on` : Имя числовой колонки (обычно ID) для параллельной выгрузки (нарезки).
- `--partitions` : Количество потоков/партиций (работает только вместе с `--partition-on`). Если параметры партиционирования не указаны, выгрузка выполняется строго в один поток.

---

## Примеры использования

**1. Простая выгрузка в Parquet в один поток (запрос текстом):**
```bash
db-extractor --conn "postgresql://user:pass@localhost:5432/mydb" \
             --query "SELECT * FROM public.users WHERE active = true" \
             --out "users.parquet"
```

**2. Выгрузка в CSV с использованием SQL-файла:**
```bash
db-extractor --conn "oracle://user:pass@10.0.0.1:1521/ORCL" \
             --query-file "./scripts/complex_report.sql" \
             --out "report.csv"
```

**3. Многопоточная выгрузка (10 потоков) для гигантских таблиц:**
```bash
db-extractor --conn "postgresql://user:pass@localhost:5432/mydb" \
             --query "SELECT * FROM large_events" \
             --partition-on "event_id" \
             --partitions 10 \
             --out "events.parquet"
```
