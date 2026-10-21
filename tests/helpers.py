"""Общие помощники для тестов db-extractor.

Тесты написаны на стандартном unittest (pytest тоже их запускает).
Тесты, которым нужен настоящий polars / connectorx, помечены skipUnless.
"""
import argparse
import contextlib
import importlib.util
import io
import os
import shutil
import sys
import tempfile
import unittest
from unittest import mock

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MAIN_PATH = os.path.join(ROOT, "src-python", "main.py")


def _load_main():
    spec = importlib.util.spec_from_file_location("dbx_main", MAIN_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


dbx = _load_main()
pl = dbx.pl

# Настоящий polars имеет строковую __version__; заглушка (MagicMock) нет.
HAS_POLARS = isinstance(getattr(pl, "__version__", None), str)
HAS_CONNECTORX = HAS_POLARS and importlib.util.find_spec("connectorx") is not None

needs_polars = unittest.skipUnless(HAS_POLARS, "requires real polars")
needs_connectorx = unittest.skipUnless(HAS_CONNECTORX, "requires real polars and connectorx")


def ns(**kwargs):
    """Namespace с теми же полями, что и argparse в main()."""
    defaults = dict(
        out=None, format=None, compression=None, partition_on=None, partitions=None,
        fill_nulls=None, drop_nulls=None, utc=False, categorize=None, hash_columns=None,
        cast_universal=False, decimal_as=None, quiet=False,
    )
    defaults.update(kwargs)
    return argparse.Namespace(**defaults)


class FakePanic(BaseException):
    """Имитация pyo3.PanicException: наследник BaseException, а не Exception."""


class FakeDF:
    """Минимальная замена DataFrame для проверки потока управления без polars."""

    def __init__(self, columns=("a",), rows=3, fail_on_write=False):
        self.columns = list(columns)
        self.schema = {c: object() for c in self.columns}
        self._rows = rows
        self.fail_on_write = fail_on_write
        self.writes = []  # (kind, path, kwargs)

    def __len__(self):
        return self._rows

    def _write(self, kind, path, **kwargs):
        self.writes.append((kind, path, kwargs))
        with open(path, "wb") as f:
            f.write(b"partial")
        if self.fail_on_write:
            raise RuntimeError("disk full")

    def write_csv(self, path, **kwargs):
        self._write("csv", path, **kwargs)

    def write_parquet(self, path, **kwargs):
        self._write("parquet", path, **kwargs)

    def write_ipc(self, path, **kwargs):
        self._write("arrow", path, **kwargs)

    def write_ndjson(self, path, **kwargs):
        self._write("jsonl", path, **kwargs)


class CliResult:
    def __init__(self, code, out, err, log, calls, dfs, directory, files):
        self.code = code
        self.out = out
        self.err = err
        self.log = log
        self.calls = calls
        self.dfs = dfs
        self.dir = directory
        self.files = files

    def path(self, name):
        return os.path.join(self.dir, name)

    def exists(self, name):
        return os.path.exists(self.path(name))


class CliTestCase(unittest.TestCase):
    """Запускает main() в чистой временной папке с подменённым чтением из БД."""

    def run_cli(self, argv, files=None, read=None, env=None, process_query=None):
        tmp = tempfile.mkdtemp(prefix="dbx_test_")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        for name, content in (files or {}).items():
            path = os.path.join(tmp, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            data = content if isinstance(content, bytes) else content.encode("utf-8")
            with open(path, "wb") as f:
                f.write(data)

        calls, dfs = [], []

        def fake_read(query=None, uri=None, engine=None, **kwargs):
            calls.append({"query": query, "uri": uri, "engine": engine, "kwargs": kwargs})
            if read is not None:
                df = read(query=query, uri=uri, engine=engine, **kwargs)
            else:
                df = FakeDF()
            dfs.append(df)
            return df

        environ = {k: v for k, v in os.environ.items() if k != "DB_EXTRACTOR_CONN"}
        environ.update(env or {})

        out, err = io.StringIO(), io.StringIO()
        old_cwd = os.getcwd()
        os.chdir(tmp)
        patches = [
            mock.patch.object(pl, "read_database_uri", fake_read),
            mock.patch.object(sys, "argv", ["db-extractor"] + list(argv)),
            mock.patch.dict(os.environ, environ, clear=True),
        ]
        if process_query is not None:
            patches.append(mock.patch.object(dbx, "process_query", process_query))
        code = 0
        try:
            with contextlib.ExitStack() as stack:
                for p in patches:
                    stack.enter_context(p)
                stack.enter_context(contextlib.redirect_stdout(out))
                stack.enter_context(contextlib.redirect_stderr(err))
                try:
                    dbx.main()
                except SystemExit as e:
                    code = 0 if e.code is None else e.code
        finally:
            os.chdir(old_cwd)

        log_path = os.path.join(tmp, "export_log.txt")
        log = ""
        if os.path.exists(log_path):
            with open(log_path, encoding="utf-8") as f:
                log = f.read()
        return CliResult(code, out.getvalue(), err.getvalue(), log, calls, dfs, tmp, sorted(os.listdir(tmp)))
