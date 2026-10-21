"""Сравнение Python- и Rust-версий db-extractor на одной SQLite-базе.

Запуск (нужны polars и connectorx; бинарник Rust собран заранее):

    python tests/parity/run_parity.py --rust src-rust/target/release/db-extractor

Для каждого сценария обе версии получают одинаковые аргументы. Сравниваются код возврата,
статус и число строк в export_log.txt, а также содержимое выходных файлов: имена колонок,
значения и «семейство» типа (целое, дробное, строка, дата, ...). Результат: таблица PASS/FAIL
и подробности расхождений. Код возврата 1, если есть расхождения.
"""
import argparse
import datetime as dt
import gzip
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import urllib.parse

import polars as pl

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PY_MAIN = os.path.join(ROOT, "src-python", "main.py")


# --------------------------------------------------------------------------
# Тестовая база
# --------------------------------------------------------------------------

def make_db(path):
    con = sqlite3.connect(path)
    con.execute(
        "CREATE TABLE people (id INTEGER, name TEXT, email TEXT, score REAL, active BOOLEAN, "
        "born DATE, created DATETIME, payload BLOB, qty INT4)"
    )
    rows = [
        (1, "Alice", "alice@example.com", 1.5, 1, "1990-05-17", "2024-01-02 03:04:05", b"\x00\x01", 10),
        (2, "Боб, \"Второй\"", "bob@example.com", None, 0, "1985-12-31", "2024-03-04 05:06:07.123456", b"", 20),
        (3, None, None, 3.25, None, None, None, None, None),
        (4, "line\nbreak", "dave@example.com", -0.5, 1, "2000-01-01", "2023-12-31 23:59:59", b"ab", 40),
        (5, "Eve", "eve@example.com", 1e-7, 0, "1999-09-09", "2024-06-30 12:00:00", None, None),
        (6, "Frank", "FRANK@EXAMPLE.COM", 123456789.125, 1, "1970-01-01", "1970-01-01 00:00:00", b"\xff", 60),
    ]
    con.executemany("INSERT INTO people VALUES (?,?,?,?,?,?,?,?,?)", rows)
    con.commit()
    con.close()


# --------------------------------------------------------------------------
# Сценарии
# --------------------------------------------------------------------------

SELECT_ALL = "select id, name, email, score, active, born, created, payload, qty from people order by id"
SELECT_BASIC = "select id, name, email, score, qty from people order by id"


def sc(name, args, outs, kind, sql=SELECT_ALL, exit_code=0, sort=False, query_file=None):
    return dict(name=name, args=args, outs=outs, kind=kind, sql=sql, exit_code=exit_code,
                sort=sort, query_file=query_file)


SCENARIOS = [
    sc("parquet default", [], ["o.parquet"], "parquet"),
    sc("parquet snappy", ["--compression", "snappy"], ["o.parquet"], "parquet"),
    sc("csv plain", [], ["o.csv"], "csv", sql=SELECT_BASIC),
    sc("csv all types", [], ["o.csv"], "csv", sql="select id, name, score, active, born, created, qty from people order by id"),
    sc("csv gzip level (README example)", ["--compression", "gzip:6"], ["o.csv"], "csv", sql=SELECT_BASIC),
    sc("csv zstd by extension", [], ["o.csv.zst"], "csv", sql=SELECT_BASIC),
    sc("jsonl", [], ["o.jsonl"], "jsonl", sql=SELECT_BASIC),
    sc("arrow", [], ["o.arrow"], "arrow", sql=SELECT_BASIC),
    sc("hash email", ["--hash-columns", "email"], ["o.parquet"], "parquet"),
    sc("hash email, case-insensitive name", ["--hash-columns", "EMAIL"], ["o.parquet"], "parquet"),
    sc("hash email with salt", ["--hash-columns", "email", "--hash-salt", "s3cret"], ["o.parquet"], "parquet"),
    sc("hash missing column fails", ["--hash-columns", "nope"], [], "parquet", exit_code=1),
    sc("fill-nulls all", ["--fill-nulls"], ["o.parquet"], "parquet"),
    sc("fill-nulls selected", ["--fill-nulls", "name", "score"], ["o.parquet"], "parquet"),
    sc("drop-nulls all", ["--drop-nulls"], ["o.parquet"], "parquet"),
    sc("drop-nulls selected", ["--drop-nulls", "email"], ["o.parquet"], "parquet"),
    sc("utc", ["--utc"], ["o.parquet"], "parquet"),
    sc("cast-universal", ["--cast-universal"], ["o.parquet"], "parquet"),
    sc("categorize", ["--categorize", "name"], ["o.parquet"], "parquet"),
    sc("partitions", ["--partition-on", "id", "--partitions", "2"], ["o.parquet"], "parquet", sort=True),
    sc("empty result parquet", [], ["o.parquet"], "parquet", sql="select id, name from people where 1 = 0"),
    sc("empty result csv", [], ["o.csv"], "csv", sql="select id, name from people where 1 = 0"),
    sc("bad format fails before the query", ["--format", "csv", "--compression", "gzip:99"], [], "csv", exit_code=1),
    sc("broken sql fails", [], [], "parquet", sql="select * from no_such_table", exit_code=1),
    sc("annotations via query file",
       [], ["a.parquet", "b.csv"], None,
       query_file=(
           "-- @DX_OUT: a.parquet\n-- @DX_HASH_COLUMNS: email\n-- @DX_FILL_NULLS\n" + SELECT_ALL + ";\n"
           "-- @DX_OUT: b.csv\n-- @DX_DROP_NULLS: name\n" + SELECT_BASIC + ";\n"
       )),
]


# --------------------------------------------------------------------------
# Запуск
# --------------------------------------------------------------------------

def run_impl(cmd_prefix, scenario, conn, workdir):
    os.makedirs(workdir, exist_ok=True)
    args = list(cmd_prefix) + ["--conn", conn]
    if scenario["query_file"] is not None:
        qf = os.path.join(workdir, "queries.sql")
        with open(qf, "w", encoding="utf-8") as f:
            f.write(scenario["query_file"])
        args += ["--query-file", qf]
    else:
        out = scenario["outs"][0] if scenario["outs"] else "o.parquet"
        args += ["--query", scenario["sql"], "--out", out]
    args += scenario["args"]
    proc = subprocess.run(args, cwd=workdir, capture_output=True, text=True, encoding="utf-8")
    log_path = os.path.join(workdir, "export_log.txt")
    log = open(log_path, encoding="utf-8").read() if os.path.exists(log_path) else ""
    return proc, log


def log_summary(log):
    """[(статус, строки)] по строкам журнала."""
    result = []
    for line in log.splitlines():
        m = re.search(r"\] (SUCCESS|ERROR) \|", line)
        if not m:
            continue
        rows = re.search(r"Rows: (\d+)", line)
        result.append((m.group(1), int(rows.group(1)) if rows else None))
    return result


# --------------------------------------------------------------------------
# Сравнение файлов
# --------------------------------------------------------------------------

def family(dtype):
    if dtype in (pl.Boolean,):
        return "bool"
    if dtype.is_integer():
        return "int"
    if dtype.is_float() or dtype == pl.Decimal or isinstance(dtype, pl.Decimal):
        return "float"
    if dtype in (pl.String, pl.Categorical) or isinstance(dtype, (pl.Categorical, pl.Enum)):
        return "str"
    if dtype == pl.Date:
        return "date"
    if isinstance(dtype, pl.Datetime):
        return "datetime-tz" if dtype.time_zone else "datetime"
    if dtype == pl.Binary:
        return "binary"
    return str(dtype)


def norm(value):
    if value is None:
        return None
    if isinstance(value, float):
        return round(value, 9)
    if isinstance(value, (bytes, bytearray)):
        return bytes(value).hex()
    if isinstance(value, dt.datetime):
        return value.astimezone(dt.timezone.utc).replace(tzinfo=None).isoformat() if value.tzinfo else value.isoformat()
    if isinstance(value, (dt.date, dt.time)):
        return value.isoformat()
    try:
        from decimal import Decimal
        if isinstance(value, Decimal):
            return round(float(value), 9)
    except Exception:
        pass
    return value


def read_file(path, kind):
    if kind == "parquet":
        return pl.read_parquet(path)
    if kind == "arrow":
        return pl.read_ipc(path)
    if kind == "jsonl":
        return pl.read_ndjson(path)
    if kind == "csv":
        return pl.read_csv(path, infer_schema=False)  # всё как текст: сравниваем формат записи
    raise ValueError(kind)


def table(df, sort):
    rows = [tuple(norm(v) for v in row) for row in df.rows()]
    if sort:
        rows.sort(key=lambda r: repr(r))
    return rows


def compare_files(py_path, rs_path, kind, sort):
    problems = []
    if not os.path.exists(py_path) or not os.path.exists(rs_path):
        return [f"missing file: python={os.path.exists(py_path)} rust={os.path.exists(rs_path)}"]
    try:
        a, b = read_file(py_path, kind), read_file(rs_path, kind)
    except Exception as e:  # noqa: BLE001
        return [f"cannot read output: {e}"]
    if a.columns != b.columns:
        problems.append(f"columns differ: python={a.columns} rust={b.columns}")
        return problems
    fa = [family(t) for t in a.schema.values()]
    fb = [family(t) for t in b.schema.values()]
    if kind != "csv" and fa != fb:
        problems.append("type families differ: " + ", ".join(
            f"{c}: py={x} rs={y}" for c, x, y in zip(a.columns, fa, fb) if x != y))
    ta, tb = table(a, sort), table(b, sort)
    if len(ta) != len(tb):
        problems.append(f"row counts differ: python={len(ta)} rust={len(tb)}")
    for i, (ra, rb) in enumerate(zip(ta, tb)):
        if ra != rb:
            diffs = [f"{a.columns[j]}: py={x!r} rs={y!r}" for j, (x, y) in enumerate(zip(ra, rb)) if x != y]
            problems.append(f"row {i}: " + "; ".join(diffs))
            if len(problems) > 6:
                problems.append("...")
                break
    return problems


def prepare_csv(path):
    """gzip-csv распаковываем рядом; zstd-csv polars читает сам."""
    with open(path, "rb") as f:
        head = f.read(4)
    if head[:2] == b"\x1f\x8b":
        target = path + ".plain"
        with gzip.open(path, "rb") as src, open(target, "wb") as dst:
            shutil.copyfileobj(src, dst)
        return target
    return path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust", required=True, help="path to the Rust db-extractor binary")
    parser.add_argument("--only", help="substring of the scenario name")
    parser.add_argument("--keep", action="store_true", help="keep the work directory")
    opts = parser.parse_args()

    work = tempfile.mkdtemp(prefix="dbx-parity-")
    db = os.path.join(work, "people.db")
    make_db(db)
    conn = "sqlite://" + urllib.parse.quote(db)

    py_cmd = [sys.executable, PY_MAIN, "-q"]
    rs_cmd = [os.path.abspath(opts.rust), "-q"]

    failures = 0
    for index, scenario in enumerate(SCENARIOS):
        if opts.only and opts.only not in scenario["name"]:
            continue
        base = os.path.join(work, f"s{index:02d}")
        py_proc, py_log = run_impl(py_cmd, scenario, conn, os.path.join(base, "py"))
        rs_proc, rs_log = run_impl(rs_cmd, scenario, conn, os.path.join(base, "rs"))

        problems = []
        if py_proc.returncode != scenario["exit_code"]:
            problems.append(f"python exit code {py_proc.returncode}, expected {scenario['exit_code']}: "
                            f"{(py_proc.stdout + py_proc.stderr).strip()[:300]}")
        if rs_proc.returncode != scenario["exit_code"]:
            problems.append(f"rust exit code {rs_proc.returncode}, expected {scenario['exit_code']}: "
                            f"{(rs_proc.stdout + rs_proc.stderr).strip()[:300]}")
        if log_summary(py_log) != log_summary(rs_log):
            problems.append(f"log differs: python={log_summary(py_log)} rust={log_summary(rs_log)}")

        for out in scenario["outs"]:
            kind = scenario["kind"] or ("csv" if out.endswith((".csv", ".zst", ".gz")) else "parquet")
            py_out = os.path.join(base, "py", out)
            rs_out = os.path.join(base, "rs", out)
            if kind == "csv" and os.path.exists(py_out) and os.path.exists(rs_out):
                try:
                    py_out, rs_out = prepare_csv(py_out), prepare_csv(rs_out)
                except Exception as e:  # noqa: BLE001
                    problems.append(f"cannot decompress {out}: {e}")
                    continue
            problems += [f"{out}: {p}" for p in compare_files(py_out, rs_out, kind, scenario["sort"])]

        if scenario["exit_code"] != 0:
            for impl, base_dir in (("python", "py"), ("rust", "rs")):
                leftovers = [f for f in os.listdir(os.path.join(base, base_dir))
                             if f.startswith("o.") or f.endswith(".tmp") or f.startswith(".o.")]
                if leftovers:
                    problems.append(f"{impl} left files after an error: {leftovers}")

        status = "PASS" if not problems else "FAIL"
        failures += bool(problems)
        print(f"[{status}] {scenario['name']}")
        for p in problems:
            print(f"        - {p}")

    print(f"\n{failures} scenario(s) differ. Work directory: {work}")
    if not opts.keep and failures == 0:
        shutil.rmtree(work, ignore_errors=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
