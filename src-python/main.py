import argparse
import datetime
import hashlib
import hmac
import os
import re
import sys
import threading
import urllib.parse

import polars as pl

log_lock = threading.Lock()

SUPPORTED_FORMATS = ("csv", "parquet", "arrow", "jsonl")
PARQUET_COMPRESSIONS = ("uncompressed", "snappy", "gzip", "brotli", "lz4", "zstd")
ARROW_COMPRESSIONS = ("uncompressed", "lz4", "zstd")
COMPRESSION_ALIASES = {"gz": "gzip", "zst": "zstd", "none": "uncompressed"}
LEVEL_RANGES = {"gzip": (0, 9), "zstd": (1, 22), "brotli": (0, 11)}
CONN_ENV_VAR = "DB_EXTRACTOR_CONN"
SALT_ENV_VAR = "DB_EXTRACTOR_HASH_SALT"


class QueryError(Exception):
    """Ошибка конкретного запроса, которую нужно записать в лог и вывести пользователю."""


_DOLLAR_TAG = re.compile(r"\$(?:[A-Za-z_][A-Za-z0-9_]*)?\$")
_Q_CLOSE = {"[": "]", "{": "}", "(": ")", "<": ">"}


def _is_ident_char(ch):
    return ch.isalnum() or ch in "_$#"


def _quoted_end(sql, start, quote):
    """Индекс сразу за закрывающей кавычкой; удвоенная кавычка это экранирование."""
    n = len(sql)
    pos = start + 1
    while True:
        k = sql.find(quote, pos)
        if k == -1:
            return n
        if sql[k + 1:k + 2] == quote:
            pos = k + 2
            continue
        return k + 1


def _q_literal_end(sql, i):
    """Oracle q'[...]' / nq'{...}'. Возвращает конец литерала или None, если это не он."""
    n = len(sql)
    if i == 0 or sql[i - 1] not in "qQ" or i + 1 >= n:
        return None
    before = sql[i - 2] if i > 1 else ""
    if before and _is_ident_char(before):
        # допустим только префикс nq'...', перед которым нет идентификатора
        if before not in "nN" or (i > 2 and _is_ident_char(sql[i - 3])):
            return None
    delimiter = sql[i + 1]
    if delimiter.isspace() or delimiter == "'":
        return None
    closing = _Q_CLOSE.get(delimiter, delimiter) + "'"
    k = sql.find(closing, i + 2)
    return n if k == -1 else k + 2


def _dollar_end(sql, i):
    """PostgreSQL $$...$$ / $tag$...$tag$. Возвращает конец литерала или None."""
    if i > 0 and _is_ident_char(sql[i - 1]):
        return None
    m = _DOLLAR_TAG.match(sql, i)
    if not m:
        return None
    tag = m.group(0)
    k = sql.find(tag, m.end())
    return len(sql) if k == -1 else k + len(tag)


def _scan_sql(sql):
    """Разбивает SQL на сегменты (вид, текст).

    Виды: code, string, lcomment, bcomment, sep. Конкатенация текстов всех сегментов
    даёт исходную строку. `;` вне строк и комментариев это отдельный сегмент sep.
    """
    n = len(sql)
    i = start = 0
    while i < n:
        ch = sql[i]
        nxt = sql[i + 1] if i + 1 < n else ""
        kind = end = None
        if ch == "-" and nxt == "-":
            end = sql.find("\n", i)
            end = n if end == -1 else end
            kind = "lcomment"
        elif ch == "/" and nxt == "*":
            close = sql.find("*/", i + 2)
            end = n if close == -1 else close + 2
            kind = "bcomment"
        elif ch == "'":
            end = _q_literal_end(sql, i)
            if end is None:
                end = _quoted_end(sql, i, "'")
            kind = "string"
        elif ch == '"' or ch == "`":
            end = _quoted_end(sql, i, ch)
            kind = "string"
        elif ch == "$":
            end = _dollar_end(sql, i)
            if end is not None:
                kind = "string"
        elif ch == ";":
            kind, end = "sep", i + 1
        if kind is None:
            i += 1
            continue
        if start < i:
            yield "code", sql[start:i]
        yield kind, sql[i:end]
        i = start = end
    if start < n:
        yield "code", sql[start:n]


def split_sql_queries(sql_content):
    """Делит SQL по `;`, не трогая `;` внутри строк, идентификаторов и комментариев.

    Фрагменты без SQL (пустые или состоящие только из комментариев) отбрасываются.
    """
    queries = []
    current = []
    has_code = False
    for kind, text in _scan_sql(sql_content):
        if kind == "sep":
            q = "".join(current).strip()
            if q and has_code:
                queries.append(q)
            current, has_code = [], False
            continue
        current.append(text)
        if kind == "string" or (kind == "code" and text.strip()):
            has_code = True
    q = "".join(current).strip()
    if q and has_code:
        queries.append(q)
    return queries


def strip_sql_comments(sql):
    """SQL без комментариев (комментарии заменяются пробелом)."""
    return "".join(" " if kind in ("lcomment", "bcomment") else text for kind, text in _scan_sql(sql))


def _comment_text(sql):
    """Текст всех комментариев запроса, по одному фрагменту на строку."""
    chunks = []
    for kind, text in _scan_sql(sql):
        if kind == "lcomment":
            chunks.append(text)
        elif kind == "bcomment":
            chunks.append(text[2:-2] if len(text) >= 4 and text.endswith("*/") else text[2:])
    return "\n".join(chunks)


def parse_compression(value):
    """'ZSTD:10' -> ('zstd', 10, True). Третье значение False, если уровень не число."""
    if not value:
        return None, None, True
    algo, _, level = value.partition(":")
    algo = algo.strip().lower()
    algo = COMPRESSION_ALIASES.get(algo, algo) or None
    level = level.strip()
    if not level:
        return algo, None, True
    if level.isdigit():
        return algo, int(level), True
    return algo, None, False


def _annotation(comments, name, colon_required=True):
    """Значение аннотации (в пределах её строки), None если аннотации нет."""
    colon = r"[ \t]*:" if colon_required else r"[ \t]*:?"
    m = re.search(r"--[ \t]*@DX_" + name + r"(?![A-Za-z0-9_])" + colon + r"[ \t]*(.*)$",
                  comments, re.IGNORECASE | re.MULTILINE)
    return None if m is None else m.group(1).strip()


def _flag(comments, name):
    return re.search(r"--[ \t]*@DX_" + name + r"(?![A-Za-z0-9_])", comments, re.IGNORECASE) is not None


def _name_list(text):
    return [c for c in re.split(r"[,\s]+", text.strip()) if c]


def extract_dx_params(query_str, args):
    warnings = []

    comp_val, comp_lvl, level_ok = parse_compression(args.compression)
    if not level_ok:
        warnings.append("Ignoring non-numeric compression level in --compression '%s'." % args.compression)

    params = {
        "query": query_str,
        "out": args.out,
        "format": args.format,
        "compression": comp_val,
        "compression_level": comp_lvl,
        "partition_on": args.partition_on,
        "partitions": args.partitions,
        "fill_nulls": args.fill_nulls,
        "drop_nulls": args.drop_nulls,
        "utc": args.utc,
        "categorize": args.categorize,
        "hash_columns": args.hash_columns,
        "cast_universal": args.cast_universal,
        "decimal_as": args.decimal_as,
        "warnings": warnings,
    }

    comments = _comment_text(query_str)

    def value_of(name):
        """Непустое значение аннотации или None (пустое значение игнорируется с предупреждением)."""
        value = _annotation(comments, name)
        if value is None:
            return None
        if not value:
            warnings.append("Ignoring @DX_%s: the value is empty." % name)
            return None
        return value

    value = value_of("OUT")
    if value:
        params["out"] = value

    value = value_of("FORMAT")
    if value:
        params["format"] = value.split()[0].lower()

    value = value_of("COMPRESSION")
    if value:
        ann_algo, ann_lvl, ann_ok = parse_compression(value.split()[0])
        if not ann_ok:
            warnings.append("Ignoring non-numeric compression level in @DX_COMPRESSION '%s'." % value)
        if ann_algo:
            if ann_lvl is None and ann_algo == params["compression"]:
                ann_lvl = params["compression_level"]
            params["compression"], params["compression_level"] = ann_algo, ann_lvl

    value = value_of("PARTITION_ON")
    if value:
        params["partition_on"] = value.split()[0]

    value = value_of("PARTITIONS")
    if value:
        if value.isdigit():
            params["partitions"] = int(value)
        else:
            warnings.append("Ignoring @DX_PARTITIONS: '%s' is not an integer." % value)

    for name, key in (("FILL_NULLS", "fill_nulls"), ("DROP_NULLS", "drop_nulls")):
        value = _annotation(comments, name, colon_required=False)
        if value is not None:
            params[key] = _name_list(value)  # пустой список означает «все колонки»

    if _flag(comments, "UTC"):
        params["utc"] = True

    for name, key in (("CATEGORIZE", "categorize"), ("HASH_COLUMNS", "hash_columns")):
        value = value_of(name)
        if value:
            params[key] = _name_list(value)

    if _flag(comments, "CAST_UNIVERSAL"):
        params["cast_universal"] = True

    value = value_of("DECIMAL_AS")
    if value:
        mode = value.split()[0].lower()
        if mode in DECIMAL_MODES:
            params["decimal_as"] = mode
        else:
            warnings.append("Ignoring @DX_DECIMAL_AS: '%s' is not one of %s." % (value, ", ".join(DECIMAL_MODES)))

    return params


DECIMAL_MODES = ("float", "decimal", "string")


def decimal_expr(col, dtype, mode):
    """Выражение для Decimal-колонки: float (Int64 при масштабе 0 и до 18 знаков), string (точный текст)."""
    if mode == "string":
        return pl.col(col).cast(pl.String)
    if dtype.scale == 0 and dtype.precision is not None and dtype.precision <= 18:
        return pl.col(col).cast(pl.Int64)
    return pl.col(col).cast(pl.String).cast(pl.Float64, strict=False)


def hash_text(value, salt=None):
    """SHA-256 текста в hex; с солью это HMAC-SHA256. None остаётся None."""
    if value is None:
        return None
    data = bytes(value) if isinstance(value, (bytes, bytearray)) else value.encode("utf-8")
    if salt:
        return hmac.new(salt.encode("utf-8"), data, hashlib.sha256).hexdigest()
    return hashlib.sha256(data).hexdigest()


def mask_secrets(text, conn):
    """Убирает пароль из текста ошибки: и по строке подключения, и по общим шаблонам."""
    text = str(text)
    if conn:
        try:
            parts = urllib.parse.urlsplit(conn)
            secrets = {parts.password, urllib.parse.unquote(parts.password or "")}
        except ValueError:
            secrets = set()
        for secret in sorted((s for s in secrets if s), key=len, reverse=True):
            text = text.replace(secret, "***")
    text = re.sub(r"(://[^/\s:@]*):[^/\s@]*@", r"\1:***@", text)
    return re.sub(r"(?i)\b(password|pwd)=[^&\s;]*", r"\1=***", text)


def warn(args, message):
    if not args.quiet:
        print("Warning: %s" % message, file=sys.stderr)


def log_export(status, query, start_time, end_time, out_file=None, row_count=0, error_msg=None):
    log_file = "export_log.txt"
    duration = (end_time - start_time).total_seconds()
    file_size_mb = 0
    if out_file and os.path.exists(out_file):
        file_size_mb = os.path.getsize(out_file) / (1024 * 1024)

    plain = " ".join(strip_sql_comments(query).split()) or " ".join(query.split())
    clean_query = plain[:100] + ("..." if len(plain) > 100 else "")
    timestamp = start_time.strftime("%Y-%m-%d %H:%M:%S")
    error_line = " ".join(str(error_msg).split())

    with log_lock:
        with open(log_file, "a", encoding="utf-8") as f:
            if status == "SUCCESS":
                f.write(f"[{timestamp}] SUCCESS | Time: {duration:.2f}s | Rows: {row_count} | Size: {file_size_mb:.2f} MB | File: {out_file} | Query: {clean_query}\n")
            else:
                f.write(f"[{timestamp}] ERROR | Time: {duration:.2f}s | Error: {error_line} | Query: {clean_query}\n")


def resolve_columns(requested, available):
    """Сопоставляет запрошенные имена колонок с фактическими без учёта регистра.

    Точное совпадение приоритетнее; неоднозначное совпадение по регистру считается
    ненайденным. Возвращает (найденные фактические имена без повторов, ненайденные).
    """
    available = list(available)
    exact = set(available)
    by_lower = {}
    for name in available:
        by_lower.setdefault(name.lower(), []).append(name)
    found, missing = [], []
    for name in requested:
        if name in exact:
            actual = name
        elif len(by_lower.get(name.lower(), [])) == 1:
            actual = by_lower[name.lower()][0]
        else:
            missing.append(name)
            continue
        if actual not in found:
            found.append(actual)
    return found, missing


def _resolve_format(item, out):
    fmt = item["format"]
    if fmt:
        fmt = fmt.lower()
        if fmt not in SUPPORTED_FORMATS:
            raise QueryError("Unsupported format '%s' (supported: %s)." % (fmt, ", ".join(SUPPORTED_FORMATS)))
        return fmt
    base_name = re.sub(r'\.(gz|zstd|zst|zip)$', '', out, flags=re.IGNORECASE)
    ext = os.path.splitext(base_name)[1].lower()
    if ext == ".csv":
        return "csv"
    if ext == ".parquet":
        return "parquet"
    if ext in (".arrow", ".ipc"):
        return "arrow"
    if ext in (".jsonl", ".ndjson"):
        return "jsonl"
    raise QueryError("Could not infer format from output file '%s'." % out)


def _resolve_compression(item, out, fmt, args):
    """Возвращает (алгоритм или None, уровень). Не поддерживаемое форматом сжатие отключается с предупреждением."""
    comp = item["compression"]
    level = item["compression_level"]
    lower_out = out.lower()
    if not comp:
        if lower_out.endswith(".gz"):
            comp = "gzip"
        elif lower_out.endswith(".zst") or lower_out.endswith(".zstd"):
            comp = "zstd"
        elif lower_out.endswith(".zip"):
            warn(args, "zip compression is not supported; '%s' is written uncompressed." % out)

    if fmt == "parquet":
        comp = comp or "zstd"
        if comp not in PARQUET_COMPRESSIONS:
            raise QueryError("Unsupported compression '%s' for parquet (supported: %s)."
                             % (comp, ", ".join(PARQUET_COMPRESSIONS)))
    elif fmt == "csv":
        if comp in (None, "uncompressed"):
            comp = None
        elif comp not in ("gzip", "zstd"):
            warn(args, "CSV compression '%s' is not supported (use gzip or zstd); writing uncompressed." % comp)
            comp = None
    elif fmt == "arrow":
        if comp in (None, "uncompressed"):
            comp = "uncompressed"
        elif comp not in ARROW_COMPRESSIONS:
            warn(args, "Arrow compression '%s' is not supported (use lz4 or zstd); writing uncompressed." % comp)
            comp = "uncompressed"
    else:  # jsonl
        if comp not in (None, "uncompressed"):
            warn(args, "JSONL compression '%s' is not supported and is ignored." % comp)
        comp = None

    # Уровень проверяется до обращения к БД; алгоритмы без уровней его игнорируют.
    if comp is None:
        level = None
    elif level is not None:
        if comp in LEVEL_RANGES:
            low, high = LEVEL_RANGES[comp]
            if not low <= level <= high:
                raise QueryError("Compression level %d is out of range for %s (%d-%d)." % (level, comp, low, high))
        else:
            warn(args, "Compression level is ignored: '%s' has no levels." % comp)
            level = None
    return comp, level


def _write_atomic(out, writer):
    """Пишет во временный файл рядом с целевым и переименовывает после успеха."""
    if os.path.islink(out) or (os.path.exists(out) and not os.path.isfile(out)):
        writer(out)  # устройство, канал или ссылка (например /dev/stdout): пишем напрямую, без rename
        return
    out_dir = os.path.dirname(os.path.abspath(out))
    tmp = os.path.join(out_dir, ".%s.%d.%d.tmp" % (os.path.basename(out), os.getpid(), threading.get_ident()))
    try:
        writer(tmp)
        os.replace(tmp, out)
    except BaseException:
        try:
            os.remove(tmp)
        except OSError:
            pass
        raise


def _select_columns(args, df, requested, option):
    """Находит колонки по имени без учёта регистра; ненайденные только предупреждение."""
    found, missing = resolve_columns(requested, df.columns)
    if missing:
        warn(args, "Column(s) not found and skipped for %s: %s" % (option, ", ".join(missing)))
    return found


def _process_query(item, idx, total, args):
    current_sql = item["query"]
    current_out = item["out"]

    for message in item.get("warnings", []):
        warn(args, message)

    fmt = _resolve_format(item, current_out)
    compression, compression_level = _resolve_compression(item, current_out, fmt, args)

    kwargs = {}
    if item["partition_on"] and item["partitions"]:
        kwargs["partition_on"] = item["partition_on"]
        kwargs["partition_num"] = item["partitions"]
    elif item["partition_on"] or item["partitions"]:
        warn(args, "Partitioning is disabled: --partition-on and --partitions must be used together.")

    out_dir = os.path.dirname(current_out)
    if out_dir:
        try:
            os.makedirs(out_dir, exist_ok=True)
        except OSError as e:
            raise QueryError("Cannot create output directory '%s': %s" % (out_dir, e))

    if not args.quiet:
        print(f"\n[{idx+1}/{total}] Executing query -> {current_out} ...")
        if "partition_on" in kwargs:
            print(f"  -> Partitioning enabled: on '{item['partition_on']}' with {item['partitions']} partitions")

    start_time = datetime.datetime.now()
    # connectorx оборачивает запрос в подзапрос без перевода строки: завершающий `-- комментарий`
    # (например аннотация) «съел» бы закрывающую скобку.
    db_query = current_sql + "\n" if "partition_on" in kwargs else current_sql
    df = pl.read_database_uri(
        query=db_query,
        uri=args.conn,
        engine="connectorx",
        **kwargs
    )

    if item["hash_columns"]:
        columns, missing = resolve_columns(item["hash_columns"], df.columns)
        if missing:
            raise QueryError("Column(s) to hash not found in the result: %s. Nothing was written, "
                             "because unmasked data would be exposed." % ", ".join(missing))
        hash_salt = getattr(args, "hash_salt", None) or os.environ.get(SALT_ENV_VAR)
        for col in columns:
            source = pl.col(col) if df.schema[col] == pl.Binary else pl.col(col).cast(pl.String)
            df = df.with_columns(
                source.map_elements(
                    lambda x: hash_text(x, hash_salt),
                    return_dtype=pl.String
                ).alias(col)
            )

    if item["utc"]:
        for col, dtype in df.schema.items():
            if isinstance(dtype, pl.Datetime):
                if dtype.time_zone is None:
                    df = df.with_columns(pl.col(col).dt.replace_time_zone("UTC"))
                else:
                    df = df.with_columns(pl.col(col).dt.convert_time_zone("UTC"))

    if item["categorize"]:
        categorize_cols = _select_columns(args, df, item["categorize"], "--categorize")
        if fmt == "parquet":  # в остальных форматах значения и так строки; в Arrow IPC словари не нужны
            for col in categorize_cols:
                df = df.with_columns(pl.col(col).cast(pl.Categorical))

    if item.get("fill_nulls") is not None:
        if len(item["fill_nulls"]) == 0:
            target_cols = df.columns
        else:
            target_cols = _select_columns(args, df, item["fill_nulls"], "--fill-nulls")

        for c in target_cols:
            dtype = df.schema[c]
            if dtype == pl.String or dtype == pl.Categorical:
                df = df.with_columns(pl.col(c).fill_null("null"))
            elif dtype.is_numeric():
                df = df.with_columns(pl.col(c).fill_null(0))

    if item["drop_nulls"] is not None:
        if len(item["drop_nulls"]) == 0:
            df = df.drop_nulls()
        else:
            valid_cols = _select_columns(args, df, item["drop_nulls"], "--drop-nulls")
            if valid_cols:
                df = df.drop_nulls(subset=valid_cols)

    decimal_mode = item.get("decimal_as") or ("float" if item.get("cast_universal") else "decimal")
    dec_exprs = [decimal_expr(col, dtype, decimal_mode) for col, dtype in df.schema.items()
                 if isinstance(dtype, pl.Decimal) and decimal_mode != "decimal"]
    if dec_exprs:
        df = df.with_columns(dec_exprs)

    if item.get("cast_universal"):
        uni_exprs = []
        for col, dtype in df.schema.items():
            if isinstance(dtype, pl.Datetime):
                c = pl.col(col).dt.cast_time_unit("us")
                if dtype.time_zone is not None:
                    c = c.dt.convert_time_zone("UTC").dt.replace_time_zone(None)
                uni_exprs.append(c)
            elif isinstance(dtype, (pl.List, pl.Array, pl.Struct)):
                # Вложенные типы как JSON-строки (NULL остаётся NULL).
                encoded = (pl.struct(pl.col(col).alias("c")).struct.json_encode()
                           .str.strip_prefix('{"c":').str.strip_suffix("}"))
                uni_exprs.append(
                    pl.when(pl.col(col).is_null()).then(pl.lit(None, dtype=pl.String)).otherwise(encoded).alias(col))
            elif dtype in (pl.Object, pl.Unknown, pl.Categorical, pl.Int128) or isinstance(dtype, (pl.Duration, pl.Enum)):
                uni_exprs.append(pl.col(col).cast(pl.String))
        if uni_exprs:
            df = df.with_columns(uni_exprs)

    if fmt in ("csv", "jsonl"):
        binary_cols = [c for c, t in df.schema.items() if t == pl.Binary]
        if binary_cols:
            df = df.with_columns([pl.col(c).bin.encode("hex") for c in binary_cols])

    row_count = len(df)
    if not args.quiet:
        print(f"Extracted and processed {row_count} rows. Saving to {current_out}...")

    if fmt == "csv":
        csv_kwargs = {}
        if compression:
            csv_kwargs["compression"] = compression
            if compression_level is not None:
                csv_kwargs["compression_level"] = compression_level
            # Имя файла задаёт пользователь (например users.csv + gzip), polars его не проверяет.
            csv_kwargs["check_extension"] = False
        _write_atomic(current_out, lambda path: df.write_csv(path, **csv_kwargs))
    elif fmt == "parquet":
        pq_kwargs = {"compression": compression}
        if compression_level is not None:
            pq_kwargs["compression_level"] = compression_level
        _write_atomic(current_out, lambda path: df.write_parquet(path, **pq_kwargs))
    elif fmt == "arrow":
        _write_atomic(current_out, lambda path: df.write_ipc(path, compression=compression))
    else:
        _write_atomic(current_out, lambda path: df.write_ndjson(path))

    end_time = datetime.datetime.now()
    log_export("SUCCESS", current_sql, start_time, end_time, current_out, row_count)
    if not args.quiet:
        print(f"Done saving {current_out}!")

    del df


def process_query(item, idx, total, args):
    start_time = datetime.datetime.now()
    try:
        _process_query(item, idx, total, args)
        return True
    except (KeyboardInterrupt, SystemExit):
        raise
    except BaseException as e:
        # BaseException, потому что паника Rust-кода (connectorx) приходит как PanicException,
        # который не является наследником Exception.
        end_time = datetime.datetime.now()
        message = " ".join(mask_secrets(e if str(e) else type(e).__name__, args.conn).split())
        log_export("ERROR", item["query"], start_time, end_time, error_msg=message)
        if not args.quiet:
            print(f"Error on query {idx+1}: {message}")
        return False


def _read_text(path, encoding):
    with open(path, "r", encoding=encoding) as f:
        return f.read()


def _sql_files(path):
    """Все .sql в папке (в том числе вложенных) в стабильном порядке; расширение без учёта регистра."""
    found = []
    for root, dirs, files in os.walk(path):
        dirs.sort(key=str.lower)
        for name in sorted(files, key=str.lower):
            if name.lower().endswith(".sql"):
                found.append(os.path.join(root, name))
    return found


def main():
    parser = argparse.ArgumentParser(description="Fast DB Extractor using Polars + ConnectorX")
    parser.add_argument("--conn", help="Database connection string (e.g., postgresql://user:pass@host:port/db)")
    parser.add_argument("--conn-file", help="Path to file containing the database connection string")

    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--query", help="SQL query string")
    group.add_argument("--query-file", help="Path to .sql file containing the query")

    parser.add_argument("--out", required=False, help="Output file path (default if not specified in query)")
    parser.add_argument("--format", choices=["csv", "parquet", "arrow", "jsonl"], help="Output format (default: infer from output extension)")
    parser.add_argument("--compression", help="Compression algorithm and optional level (e.g., 'zstd', 'zstd:10', 'gzip')")
    parser.add_argument("--fill-nulls", nargs='*', help="Fill nulls with default values ('null' for text, 0 for numbers) in specified columns (or all if not specified)")
    parser.add_argument("--drop-nulls", nargs='*', help="Drop rows with nulls in specified columns (or all if no columns specified)")
    parser.add_argument("--utc", action="store_true", help="Convert datetimes to UTC")
    parser.add_argument("--categorize", nargs='+', help="List of columns to convert to categorical")
    parser.add_argument("--hash-columns", nargs='+', help="List of columns to hash (e.g. for PII)")
    parser.add_argument("--hash-salt", help="Secret salt for --hash-columns: values are hashed with HMAC-SHA256 instead of plain SHA-256 (or set %s)" % SALT_ENV_VAR)
    parser.add_argument("--decimal-as", choices=DECIMAL_MODES, default=None, help="Representation of Decimal columns: float (default with --cast-universal; Int64 for scale 0 and up to 18 digits), decimal (unchanged; default without --cast-universal), string (exact text)")
    parser.add_argument("--cast-universal", action="store_true", help="Cast complex/database-specific types (Decimal, Datetime, Objects) to universal Parquet/Arrow compatible types")
    parser.add_argument("--partition-on", help="Column name to partition on (for connectorx)")
    parser.add_argument("--partitions", type=int, help="Number of partitions (requires --partition-on)")
    parser.add_argument("--continue-on-error", action="store_true", help="Continue executing remaining queries if one fails")
    parser.add_argument("--workers", type=int, default=1, help="Number of concurrent workers (Thread Pool)")
    parser.add_argument("--encoding", default="utf-8-sig", help="Encoding of query files (default: utf-8-sig, i.e. UTF-8 with optional BOM)")
    parser.add_argument("-q", "--quiet", action="store_true", help="Suppress console output")

    args = parser.parse_args()

    env_conn = os.environ.get(CONN_ENV_VAR)
    if not args.conn and not args.conn_file and not env_conn:
        parser.error("Either --conn or --conn-file must be provided (or set %s)." % CONN_ENV_VAR)

    if args.conn_file:
        if args.conn:
            warn(args, "Both --conn and --conn-file are given; --conn-file is used.")
        try:
            args.conn = _read_text(args.conn_file, "utf-8-sig").strip()
        except Exception as e:
            if not args.quiet: print(f"Error reading connection file '{args.conn_file}': {e}")
            sys.exit(1)
    elif not args.conn:
        args.conn = env_conn.strip()

    if not args.conn:
        if not args.quiet: print("Error: the connection string is empty.")
        sys.exit(1)

    queries_to_run = []

    if args.query_file:
        if os.path.isdir(args.query_file):
            files_to_process = _sql_files(args.query_file)
        elif os.path.isfile(args.query_file):
            files_to_process = [args.query_file]
        else:
            if not args.quiet: print(f"Error: Path '{args.query_file}' does not exist.")
            sys.exit(1)

        for fp in files_to_process:
            try:
                sql_content = _read_text(fp, args.encoding)
            except Exception as e:
                if not args.quiet: print(f"Error reading query file {fp}: {e}")
                sys.exit(1)

            raw_queries = split_sql_queries(sql_content)
            if not raw_queries:
                warn(args, f"No SQL statements found in {fp}; skipped.")

            for clean_q in raw_queries:
                item = extract_dx_params(clean_q, args)

                if not item["out"]:
                    if not args.quiet: print(f"Error: Output file not specified for a query in {fp}:\n{clean_q[:50]}...\nPlease specify --out argument or add '-- @DX_OUT: filename.parquet' to the query.")
                    sys.exit(1)

                queries_to_run.append(item)

        if not queries_to_run:
            if not args.quiet: print(f"Error: no SQL queries found in '{args.query_file}'.")
            sys.exit(1)
    else:
        statements = split_sql_queries(args.query)
        if len(statements) != 1:
            if not args.quiet:
                if statements:
                    print("Error: --query accepts a single statement; use --query-file for several queries.")
                else:
                    print("Error: --query contains no SQL statement.")
            sys.exit(1)

        item = extract_dx_params(statements[0], args)
        if not item["out"]:
            if not args.quiet: print("Error: --out argument is required when using --query (or add '-- @DX_OUT: filename.parquet' to the query)")
            sys.exit(1)
        queries_to_run.append(item)

    seen_outputs = {}
    for idx, item in enumerate(queries_to_run):
        key = os.path.normcase(os.path.abspath(item["out"]))
        if key in seen_outputs:
            if not args.quiet:
                print(f"Error: queries {seen_outputs[key] + 1} and {idx + 1} write to the same file '{item['out']}'. "
                      "Give each query its own '-- @DX_OUT: filename' or use --query-file with separate outputs.")
            sys.exit(1)
        seen_outputs[key] = idx

    if not args.quiet:
        print(f"Connecting to {args.conn.split('@')[-1] if '@' in args.conn else 'database'}...")

    total = len(queries_to_run)
    has_errors = False

    if args.workers > 1:
        from concurrent.futures import ThreadPoolExecutor, as_completed
        executor = ThreadPoolExecutor(max_workers=args.workers)
        try:
            futures = []
            for idx, item in enumerate(queries_to_run):
                futures.append(executor.submit(process_query, item, idx, total, args))

            for future in as_completed(futures):
                success = future.result()
                if not success:
                    has_errors = True
                    if not args.continue_on_error:
                        # Не запускаем ещё не начатые запросы, дожидаемся только уже идущих
                        executor.shutdown(wait=True, cancel_futures=True)
                        sys.exit(1)
        finally:
            executor.shutdown(wait=True)
    else:
        for idx, item in enumerate(queries_to_run):
            success = process_query(item, idx, total, args)
            if not success:
                has_errors = True
                if not args.continue_on_error:
                    sys.exit(1)

    if has_errors:
        sys.exit(1)

if __name__ == "__main__":
    main()
