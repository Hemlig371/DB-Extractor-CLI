"""Поток управления main()/process_query: ошибки, логи, запись файлов.

БД и polars здесь не нужны: чтение из БД подменяется, DataFrame заменён на FakeDF.
"""
import os
import time
import threading
import unittest

from helpers import CliTestCase, FakeDF, FakePanic

CONN = "postgresql://user:pass@host:5432/db"
BASE = ["--conn", CONN]


class HappyPath(CliTestCase):
    def test_single_query_success(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet"])
        self.assertEqual(r.code, 0)
        self.assertEqual(len(r.calls), 1)
        self.assertEqual(r.calls[0]["engine"], "connectorx")
        self.assertEqual(r.calls[0]["uri"], CONN)
        self.assertTrue(r.exists("o.parquet"))
        self.assertIn("SUCCESS", r.log)
        self.assertIn("Rows: 3", r.log)
        self.assertIn("File: o.parquet", r.log)

    def test_connection_message_hides_credentials(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet"])
        self.assertIn("Connecting to host:5432/db", r.out)
        self.assertNotIn("pass", r.out)

    def test_conn_file_with_bom_and_trailing_newline(self):
        r = self.run_cli(["--conn-file", "c.txt", "--query", "select 1", "--out", "o.parquet"],
                         files={"c.txt": b"\xef\xbb\xbfpostgresql://u:p@h/db\n"})
        self.assertEqual(r.code, 0)
        self.assertEqual(r.calls[0]["uri"], "postgresql://u:p@h/db")

    def test_connection_from_environment(self):
        r = self.run_cli(["--query", "select 1", "--out", "o.parquet"],
                         env={"DB_EXTRACTOR_CONN": "postgresql://e:e@h/db"})
        self.assertEqual(r.code, 0)
        self.assertEqual(r.calls[0]["uri"], "postgresql://e:e@h/db")

    def test_missing_connection_is_usage_error(self):
        r = self.run_cli(["--query", "select 1", "--out", "o.parquet"])
        self.assertEqual(r.code, 2)
        self.assertEqual(r.calls, [])


class FormatAndCompressionValidation(CliTestCase):
    def test_invalid_dx_format_is_an_error_not_a_success(self):
        r = self.run_cli(BASE + ["--query", "-- @DX_FORMAT: xlsx\nselect 1", "--out", "x.xlsx"])
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [], "must fail before querying the database")
        self.assertIn("ERROR", r.log)
        self.assertIn("xlsx", r.log)
        self.assertNotIn("SUCCESS", r.log)
        self.assertFalse(r.exists("x.xlsx"))

    def test_unknown_extension_is_logged(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "data.bin", "-q"])
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])
        self.assertIn("ERROR", r.log)

    def test_invalid_parquet_compression_fails_before_query(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--compression", "foo"])
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])
        self.assertIn("ERROR", r.log)

    def test_unwritable_output_directory_fails_before_query(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "blocker/sub/o.parquet"],
                         files={"blocker": "i am a file, not a directory"})
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])
        self.assertIn("ERROR", r.log)


class QueryCollection(CliTestCase):
    def test_query_option_strips_trailing_semicolon(self):
        r = self.run_cli(BASE + ["--query", "select 1;", "--out", "o.parquet"])
        self.assertEqual(r.code, 0)
        self.assertEqual(r.calls[0]["query"], "select 1")

    def test_query_option_rejects_multiple_statements(self):
        r = self.run_cli(BASE + ["--query", "select 1; select 2", "--out", "o.parquet"])
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])

    def test_query_option_with_only_comments_is_rejected(self):
        r = self.run_cli(BASE + ["--query", "-- nothing", "--out", "o.parquet"])
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])

    def test_trailing_comment_in_file_is_not_executed(self):
        r = self.run_cli(BASE + ["--query-file", "q.sql"],
                         files={"q.sql": "-- @DX_OUT: a.parquet\nselect 1;\n-- конец файла\n"})
        self.assertEqual(r.code, 0)
        self.assertEqual([c["query"].split("\n")[-1] for c in r.calls], ["select 1"])

    def test_two_queries_to_one_output_are_rejected(self):
        r = self.run_cli(BASE + ["--query-file", "q.sql", "--out", "same.parquet"],
                         files={"q.sql": "select 1; select 2;"})
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [], "must fail before querying the database")

    def test_two_different_spellings_of_one_path_are_rejected(self):
        r = self.run_cli(BASE + ["--query-file", "q.sql"],
                         files={"q.sql": "-- @DX_OUT: a.parquet\nselect 1;\n-- @DX_OUT: ./a.parquet\nselect 2;"})
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])

    def test_folder_without_sql_files_is_an_error(self):
        r = self.run_cli(BASE + ["--query-file", "qs", "--out", "o.parquet"], files={"qs/readme.txt": "x"})
        self.assertEqual(r.code, 1)
        self.assertEqual(r.calls, [])

    def test_missing_query_path_is_an_error(self):
        r = self.run_cli(BASE + ["--query-file", "nope.sql", "--out", "o.parquet"])
        self.assertEqual(r.code, 1)

    def test_folder_is_processed_sorted_and_extension_case_insensitive(self):
        files = {"qs/b.sql": "-- @DX_OUT: b.parquet\nselect 'b'",
                 "qs/a.sql": "-- @DX_OUT: a.parquet\nselect 'a'",
                 "qs/C.SQL": "-- @DX_OUT: c.parquet\nselect 'c'",
                 "qs/sub/d.sql": "-- @DX_OUT: d.parquet\nselect 'd'",
                 "qs/skip.txt": "select 'no'"}
        r = self.run_cli(BASE + ["--query-file", "qs"], files=files)
        self.assertEqual(r.code, 0)
        order = [c["query"].split("select ")[1] for c in r.calls]
        self.assertEqual(order, ["'a'", "'b'", "'c'", "'d'"])

    def test_file_with_only_comments_inside_folder_is_skipped(self):
        files = {"qs/a.sql": "-- @DX_OUT: a.parquet\nselect 1", "qs/empty.sql": "-- nothing\n"}
        r = self.run_cli(BASE + ["--query-file", "qs"], files=files)
        self.assertEqual(r.code, 0)
        self.assertEqual(len(r.calls), 1)

    def test_bom_is_not_sent_to_the_database(self):
        r = self.run_cli(BASE + ["--query-file", "q.sql"],
                         files={"q.sql": b"\xef\xbb\xbf-- @DX_OUT: o.parquet\nselect 1"})
        self.assertEqual(r.code, 0)
        self.assertNotIn("﻿", r.calls[0]["query"])

    def test_cp1251_needs_encoding_option(self):
        data = "-- комментарий\nselect 1".encode("cp1251")
        bad = self.run_cli(BASE + ["--query-file", "q.sql", "--out", "o.parquet"], files={"q.sql": data})
        self.assertEqual(bad.code, 1)
        good = self.run_cli(BASE + ["--query-file", "q.sql", "--out", "o.parquet", "--encoding", "cp1251"],
                            files={"q.sql": data})
        self.assertEqual(good.code, 0)
        self.assertEqual(len(good.calls), 1)


class Partitioning(CliTestCase):
    def test_both_options_are_passed_to_connectorx(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet",
                                 "--partition-on", "id", "--partitions", "4"])
        self.assertEqual(r.calls[0]["kwargs"], {"partition_on": "id", "partition_num": 4})

    def test_only_one_option_warns_and_disables_partitioning(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--partitions", "4"])
        self.assertEqual(r.code, 0)
        self.assertEqual(r.calls[0]["kwargs"], {})
        self.assertIn("partition", r.err.lower())

    def test_warning_is_silenced_by_quiet(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--partitions", "4", "-q"])
        self.assertEqual(r.err, "")


class ErrorHandling(CliTestCase):
    QUERIES = "-- @DX_OUT: a.parquet\nselect 1;\n-- @DX_OUT: b.parquet\nselect 2"

    def test_database_error_is_logged_and_fails(self):
        def boom(**kw):
            raise RuntimeError("ORA-00942: table or view does not exist")
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet"], read=boom)
        self.assertEqual(r.code, 1)
        self.assertIn("ERROR", r.log)
        self.assertIn("ORA-00942", r.log)

    def test_panic_is_logged_like_any_other_error(self):
        def panic(**kw):
            raise FakePanic("Type TimestampLTZ not implemented for oracle!")
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet"], read=panic)
        self.assertEqual(r.code, 1)
        self.assertIn("ERROR", r.log)
        self.assertIn("not implemented", r.log)

    def test_panic_does_not_stop_the_batch_with_continue_on_error(self):
        state = {"n": 0}

        def read(**kw):
            state["n"] += 1
            if state["n"] == 1:
                raise FakePanic("unsupported type")
            return FakeDF()
        r = self.run_cli(BASE + ["--query-file", "q.sql", "--continue-on-error"],
                         files={"q.sql": self.QUERIES}, read=read)
        self.assertEqual(r.code, 1)
        self.assertEqual(r.log.count("ERROR"), 1)
        self.assertEqual(r.log.count("SUCCESS"), 1)
        self.assertTrue(r.exists("b.parquet"))

    def test_without_continue_on_error_the_batch_stops(self):
        def boom(**kw):
            raise RuntimeError("boom")
        r = self.run_cli(BASE + ["--query-file", "q.sql"], files={"q.sql": self.QUERIES}, read=boom)
        self.assertEqual(r.code, 1)
        self.assertEqual(len(r.calls), 1)

    def test_password_is_masked_in_log_and_console(self):
        for conn, secret in (("postgresql://user:s3cr3t@host/db", "s3cr3t"),
                             ("postgresql://user:pa:ss@host/db", "pa:ss"),
                             ("postgresql://user:p@ss@host/db", "p@ss")):
            with self.subTest(conn=conn):
                def boom(uri=None, **kw):
                    raise RuntimeError("connection to %s failed" % uri)
                r = self.run_cli(["--conn", conn, "--query", "select 1", "--out", "o.parquet"], read=boom)
                self.assertEqual(r.code, 1)
                self.assertNotIn(secret, r.log)
                self.assertNotIn(secret, r.out)
                self.assertNotIn(secret, r.err)

    def test_error_message_is_a_single_log_line(self):
        def boom(**kw):
            raise RuntimeError("line one\nline two")
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet"], read=boom)
        self.assertEqual(len(r.log.strip().splitlines()), 1)

    def test_log_query_has_no_annotations(self):
        sql = "-- @DX_UTC\n-- @DX_OUT: a.parquet\nSELECT id FROM t"
        r = self.run_cli(BASE + ["--query", sql])
        self.assertIn("Query: SELECT id FROM t", r.log)
        self.assertNotIn("@DX", r.log)


class WorkerPool(CliTestCase):
    def test_failure_cancels_queued_queries(self):
        started, lock = [], threading.Lock()

        def fake_process(item, idx, total, args):
            with lock:
                started.append(idx)
            if idx == 0:
                return False
            time.sleep(0.3)
            return True
        sql = ";\n".join("-- @DX_OUT: o%d.csv\nselect %d" % (i, i) for i in range(8))
        r = self.run_cli(BASE + ["--query-file", "q.sql", "--workers", "2", "-q"],
                         files={"q.sql": sql}, process_query=fake_process)
        self.assertEqual(r.code, 1)
        self.assertLessEqual(len(started), 4, "queued queries must be cancelled: %r" % started)

    def test_continue_on_error_runs_everything(self):
        started, lock = [], threading.Lock()

        def fake_process(item, idx, total, args):
            with lock:
                started.append(idx)
            return idx != 0
        sql = ";\n".join("-- @DX_OUT: o%d.csv\nselect %d" % (i, i) for i in range(6))
        r = self.run_cli(BASE + ["--query-file", "q.sql", "--workers", "2", "--continue-on-error", "-q"],
                         files={"q.sql": sql}, process_query=fake_process)
        self.assertEqual(r.code, 1)
        self.assertEqual(sorted(started), list(range(6)))


class ColumnSelection(CliTestCase):
    def read_columns(self, *columns):
        return lambda **kw: FakeDF(columns=columns)

    def test_missing_hash_column_is_an_error_and_nothing_is_written(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--hash-columns", "email"],
                         read=self.read_columns("ID", "NAME"))
        self.assertEqual(r.code, 1)
        self.assertFalse(r.exists("o.parquet"))
        self.assertIn("ERROR", r.log)
        self.assertIn("email", r.log)

    def test_missing_categorize_column_only_warns(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--categorize", "nope"],
                         read=self.read_columns("ID"))
        self.assertEqual(r.code, 0)
        self.assertIn("nope", r.err)

    def test_missing_fill_and_drop_columns_only_warn(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet",
                                 "--fill-nulls", "nope1", "--drop-nulls", "nope2"],
                         read=self.read_columns("ID"))
        self.assertEqual(r.code, 0)
        self.assertIn("nope1", r.err)
        self.assertIn("nope2", r.err)


class WritingFiles(CliTestCase):
    def only_write(self, r):
        self.assertEqual(len(r.dfs), 1)
        self.assertEqual(len(r.dfs[0].writes), 1)
        return r.dfs[0].writes[0]

    def test_failed_write_leaves_no_file_and_no_temp(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "x.parquet"],
                         read=lambda **kw: FakeDF(fail_on_write=True))
        self.assertEqual(r.code, 1)
        self.assertEqual(r.files, ["export_log.txt"])

    def test_failed_write_keeps_previous_good_file(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "x.parquet"],
                         files={"x.parquet": "old good data"},
                         read=lambda **kw: FakeDF(fail_on_write=True))
        self.assertEqual(r.code, 1)
        with open(r.path("x.parquet")) as f:
            self.assertEqual(f.read(), "old good data")
        self.assertEqual(r.files, ["export_log.txt", "x.parquet"])

    def test_successful_write_leaves_no_temp(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "sub/x.parquet"])
        self.assertEqual(r.code, 0)
        self.assertEqual(sorted(os.listdir(r.path("sub"))), ["x.parquet"])

    def test_log_error_names_a_write_failure_as_such(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "x.parquet"],
                         read=lambda **kw: FakeDF(fail_on_write=True))
        self.assertIn("disk full", r.log)
        self.assertNotIn("Database Error", r.out)

    # --- параметры записи ---------------------------------------------------
    def test_parquet_defaults_to_zstd(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet"])
        kind, _, kw = self.only_write(r)
        self.assertEqual((kind, kw), ("parquet", {"compression": "zstd"}))

    def test_parquet_compression_is_normalised_and_level_passed(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--compression", "ZSTD:5"])
        _, _, kw = self.only_write(r)
        self.assertEqual(kw, {"compression": "zstd", "compression_level": 5})

    def test_parquet_gz_extension_selects_gzip(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet.gz"])
        _, _, kw = self.only_write(r)
        self.assertEqual(kw["compression"], "gzip")

    def test_csv_default_is_uncompressed(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.csv"])
        kind, _, kw = self.only_write(r)
        self.assertEqual((kind, kw), ("csv", {}))

    def test_csv_gzip_with_level_under_plain_csv_name(self):
        """Пример из README: users_active.csv + gzip:6."""
        r = self.run_cli(BASE + ["--query", "-- @DX_COMPRESSION: gzip:6\nselect 1", "--out", "users_active.csv"])
        kind, _, kw = self.only_write(r)
        self.assertEqual(kind, "csv")
        self.assertEqual(kw, {"compression": "gzip", "compression_level": 6, "check_extension": False})

    def test_csv_gz_extension_selects_gzip(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.csv.gz"])
        _, _, kw = self.only_write(r)
        self.assertEqual(kw["compression"], "gzip")
        self.assertIs(kw["check_extension"], False)

    def test_csv_zstd_extension_variants(self):
        for name in ("o.csv.zst", "o.csv.zstd"):
            with self.subTest(name=name):
                r = self.run_cli(BASE + ["--query", "select 1", "--out", name])
                _, _, kw = self.only_write(r)
                self.assertEqual(kw["compression"], "zstd")
                self.assertIs(kw["check_extension"], False)

    def test_csv_unsupported_compression_warns_and_writes_plain(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.csv", "--compression", "snappy"])
        _, _, kw = self.only_write(r)
        self.assertEqual(kw, {})
        self.assertIn("snappy", r.err)

    def test_zip_extension_warns_and_writes_plain(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.csv.zip"])
        kind, _, kw = self.only_write(r)
        self.assertEqual((kind, kw), ("csv", {}))
        self.assertIn("zip", r.err.lower())

    def test_arrow_unsupported_compression_warns_and_falls_back(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.arrow", "--compression", "gzip"])
        kind, _, kw = self.only_write(r)
        self.assertEqual((kind, kw), ("arrow", {"compression": "uncompressed"}))
        self.assertIn("gzip", r.err)

    def test_arrow_lz4(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.ipc", "--compression", "lz4"])
        _, _, kw = self.only_write(r)
        self.assertEqual(kw, {"compression": "lz4"})

    def test_jsonl_compression_warns_and_is_ignored(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.jsonl", "--compression", "zstd"])
        kind, _, kw = self.only_write(r)
        self.assertEqual((kind, kw), ("jsonl", {}))
        self.assertIn("zstd", r.err)


if __name__ == "__main__":
    unittest.main()
