"""Сквозные тесты через настоящий connectorx на SQLite (нужны polars и connectorx)."""
import os
import sqlite3
import tempfile
import unittest
import urllib.parse

import contextlib
import io
import shutil
import sys
from unittest import mock

from helpers import dbx, needs_connectorx, pl


@needs_connectorx
class SqliteEndToEnd(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="dbx_sqlite_")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.db = os.path.join(self.tmp, "test.db")
        con = sqlite3.connect(self.db)
        con.execute("CREATE TABLE users (id INTEGER, name TEXT, email TEXT, price REAL, created TIMESTAMP)")
        con.executemany(
            "INSERT INTO users VALUES (?,?,?,?,?)",
            [(i, "user%d" % i, "u%d@x.com" % i, i * 1.5, "2024-01-%02d 10:00:00" % (i % 28 + 1))
             for i in range(1, 21)])
        con.commit()
        con.close()
        self.conn = "sqlite://" + urllib.parse.quote(self.db)

    def run_main(self, argv):
        old = os.getcwd()
        os.chdir(self.tmp)
        out, err = io.StringIO(), io.StringIO()
        code = 0
        try:
            with mock.patch.object(sys, "argv", ["db-extractor", "--conn", self.conn] + argv), \
                    contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                try:
                    dbx.main()
                except SystemExit as e:
                    code = 0 if e.code is None else e.code
        finally:
            os.chdir(old)
        log_path = os.path.join(self.tmp, "export_log.txt")
        log = open(log_path, encoding="utf-8").read() if os.path.exists(log_path) else ""
        return code, out.getvalue(), err.getvalue(), log

    def p(self, name):
        return os.path.join(self.tmp, name)

    def test_multi_query_file_writes_every_format(self):
        with open(self.p("q.sql"), "w", encoding="utf-8") as f:
            f.write("-- @DX_OUT: a.parquet\nSELECT id, name FROM users WHERE id <= 5;\n"
                    "-- @DX_OUT: b.csv\nSELECT id, price FROM users;\n"
                    "-- @DX_OUT: c.jsonl\nSELECT id FROM users WHERE id = 1;\n"
                    "-- trailing comment\n")
        code, out, err, log = self.run_main(["--query-file", "q.sql"])
        self.assertEqual(code, 0, msg=out + err + log)
        self.assertEqual(pl.read_parquet(self.p("a.parquet")).height, 5)
        self.assertEqual(pl.read_csv(self.p("b.csv")).height, 20)
        self.assertEqual(log.count("SUCCESS"), 3)

    def test_bad_query_with_continue_on_error(self):
        with open(self.p("q.sql"), "w", encoding="utf-8") as f:
            f.write("-- @DX_OUT: bad.parquet\nSELECT * FROM no_such_table;\n"
                    "-- @DX_OUT: ok.parquet\nSELECT id FROM users;")
        code, out, err, log = self.run_main(["--query-file", "q.sql", "--continue-on-error"])
        self.assertEqual(code, 1)
        self.assertEqual(log.count("ERROR"), 1)
        self.assertEqual(log.count("SUCCESS"), 1)
        self.assertTrue(os.path.exists(self.p("ok.parquet")))
        self.assertFalse(os.path.exists(self.p("bad.parquet")))

    def test_hash_with_different_case(self):
        code, out, err, log = self.run_main([
            "--query", "SELECT id, email AS EMAIL FROM users WHERE id = 1", "--out", "h.parquet",
            "--hash-columns", "email"])
        self.assertEqual(code, 0, msg=out + err + log)
        value = pl.read_parquet(self.p("h.parquet"))["EMAIL"][0]
        self.assertEqual(len(value), 64)
        self.assertNotIn("@", value)

    def test_partitioned_read_returns_all_rows(self):
        code, out, err, log = self.run_main([
            "--query", "SELECT id, name FROM users", "--out", "p.parquet",
            "--partition-on", "id", "--partitions", "3"])
        self.assertEqual(code, 0, msg=out + err + log)
        self.assertEqual(pl.read_parquet(self.p("p.parquet")).height, 20)

    def test_empty_result(self):
        code, out, err, log = self.run_main([
            "--query", "SELECT id, name FROM users WHERE 1 = 0", "--out", "e.parquet"])
        self.assertEqual(code, 0, msg=out + err + log)
        self.assertEqual(pl.read_parquet(self.p("e.parquet")).height, 0)

    def test_two_workers(self):
        with open(self.p("q.sql"), "w", encoding="utf-8") as f:
            f.write(";\n".join("-- @DX_OUT: w%d.parquet\nSELECT id FROM users WHERE id > %d" % (i, i)
                               for i in range(4)))
        code, out, err, log = self.run_main(["--query-file", "q.sql", "--workers", "2"])
        self.assertEqual(code, 0, msg=out + err + log)
        self.assertEqual(log.count("SUCCESS"), 4)


if __name__ == "__main__":
    unittest.main()
