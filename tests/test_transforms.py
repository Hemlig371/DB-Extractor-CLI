"""Преобразования данных и реальная запись файлов (нужен настоящий polars).

Источник данных подменён: read_database_uri возвращает DataFrame, собранный по образцу
того, что отдаёт connectorx (Decimal(38,10), naive/aware Datetime, имена в верхнем регистре).
"""
import datetime as dt
import gzip
import hashlib
import json
import unittest
from decimal import Decimal

from helpers import CliTestCase, HAS_POLARS, needs_polars, pl

CONN = "postgresql://user:pass@host:5432/db"
BASE = ["--conn", CONN]


def sha(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def decimal_series(name, values):
    return pl.Series(name, [None if v is None else Decimal(v) for v in values], dtype=pl.Decimal(38, 10))


@needs_polars
class TransformTests(CliTestCase):
    def extract(self, df, *args, out="out.parquet", sql="select 1"):
        r = self.run_cli(BASE + ["--query", sql, "--out", out] + list(args), read=lambda **kw: df)
        self.assertEqual(r.code, 0, msg=r.out + r.err + r.log)
        return r

    def read(self, r, name="out.parquet"):
        return pl.read_parquet(r.path(name))

    def test_hash_matches_sha256_and_keeps_nulls(self):
        df = pl.DataFrame({"email": ["a@x.com", None, "b@x.com"], "n": [1, 2, 3]})
        out = self.read(self.extract(df, "--hash-columns", "email"))
        self.assertEqual(out["email"].to_list(), [sha("a@x.com"), None, sha("b@x.com")])
        self.assertEqual(out["n"].to_list(), [1, 2, 3])

    def test_hash_column_name_matches_regardless_of_case(self):
        """Oracle возвращает EMAIL, пользователь пишет email."""
        df = pl.DataFrame({"EMAIL": ["a@x.com"], "ID": [7]})
        out = self.read(self.extract(df, "--hash-columns", "email"))
        self.assertEqual(out["EMAIL"].to_list(), [sha("a@x.com")])

    def test_hash_non_string_is_hashed_through_its_text_form(self):
        df = pl.DataFrame({"phone": [79001234567]})
        out = self.read(self.extract(df, "--hash-columns", "phone"))
        self.assertEqual(out["phone"].to_list(), [sha("79001234567")])

    def test_hash_with_salt_is_hmac_sha256(self):
        import hmac
        df = pl.DataFrame({"email": ["a@x.com", None]})
        out = self.read(self.extract(df, "--hash-columns", "email", "--hash-salt", "s3cret"))
        expected = hmac.new(b"s3cret", b"a@x.com", hashlib.sha256).hexdigest()
        self.assertEqual(out["email"].to_list(), [expected, None])
        self.assertNotEqual(expected, sha("a@x.com"))

    def frame_with_nulls(self):
        return pl.DataFrame({
            "s": ["a", None], "i": [1, None], "f": [1.5, None],
            "d": [dt.date(2024, 1, 1), None],
        }).with_columns(decimal_series("dec", ["1.5", None]))

    def test_fill_nulls_all_columns(self):
        out = self.read(self.extract(self.frame_with_nulls(), "--fill-nulls"))
        self.assertEqual(out["s"].to_list(), ["a", "null"])
        self.assertEqual(out["i"].to_list(), [1, 0])
        self.assertEqual(out["f"].to_list(), [1.5, 0.0])
        self.assertEqual([None if v is None else float(v) for v in out["dec"].to_list()], [1.5, 0.0])
        self.assertEqual(out["d"].to_list(), [dt.date(2024, 1, 1), None], "dates are not filled")

    def test_fill_nulls_selected_columns_only(self):
        out = self.read(self.extract(self.frame_with_nulls(), "--fill-nulls", "s"))
        self.assertEqual(out["s"].to_list(), ["a", "null"])
        self.assertEqual(out["i"].to_list(), [1, None])

    def test_fill_nulls_by_annotation_without_columns(self):
        out = self.read(self.extract(self.frame_with_nulls(), sql="-- @DX_FILL_NULLS\nselect 1"))
        self.assertEqual(out["s"].to_list(), ["a", "null"])
        self.assertEqual(out["i"].to_list(), [1, 0])

    def test_fill_nulls_column_name_case_insensitive(self):
        df = pl.DataFrame({"STATUS": ["a", None]})
        out = self.read(self.extract(df, "--fill-nulls", "status"))
        self.assertEqual(out["STATUS"].to_list(), ["a", "null"])

    def test_drop_nulls_all_and_selected(self):
        df = self.frame_with_nulls().select("s", "i")
        self.assertEqual(self.read(self.extract(df, "--drop-nulls")).height, 1)
        df2 = pl.DataFrame({"a": [1, None, 3], "b": [None, 2, 3]})
        self.assertEqual(self.read(self.extract(df2, "--drop-nulls", "a")).height, 2)

    def test_drop_nulls_by_annotation_without_columns(self):
        df = pl.DataFrame({"a": [1, None], "b": [1, 2]})
        self.assertEqual(self.read(self.extract(df, sql="-- @DX_DROP_NULLS\nselect 1")).height, 1)

    def test_utc_labels_naive_and_converts_aware(self):
        naive = dt.datetime(2024, 1, 1, 12, 0)
        aware = dt.datetime(2024, 1, 1, 12, 0, tzinfo=dt.timezone(dt.timedelta(hours=3)))
        df = pl.DataFrame({"n": [naive], "a": [aware]}).with_columns(
            pl.col("a").dt.convert_time_zone("Europe/Moscow"))
        out = self.read(self.extract(df, "--utc"))
        self.assertEqual(out.schema["n"].time_zone, "UTC")
        self.assertEqual(out.schema["a"].time_zone, "UTC")
        self.assertEqual(out["n"].to_list()[0].hour, 12, "naive value is labelled, not shifted")
        self.assertEqual(out["a"].to_list()[0].hour, 9, "aware value is converted")

    def test_categorize_keeps_values(self):
        df = pl.DataFrame({"status": ["a", "b", "a"], "x": [1, 2, 3]})
        out = self.read(self.extract(df, "--categorize", "status"))
        self.assertEqual(out["status"].cast(pl.String).to_list(), ["a", "b", "a"])

    def test_cast_universal_decimal_becomes_float64(self):
        df = pl.DataFrame({"id": [1]}).with_columns(decimal_series("amt", ["10.2500000000"]))
        out = self.read(self.extract(df, "--cast-universal"))
        self.assertEqual(out.schema["amt"], pl.Float64)
        self.assertEqual(out["amt"].to_list(), [10.25])

    def test_cast_universal_datetime_to_naive_utc_microseconds(self):
        aware = dt.datetime(2024, 1, 1, 12, 0, tzinfo=dt.timezone(dt.timedelta(hours=3)))
        df = pl.DataFrame({"a": [aware], "n": [dt.datetime(2024, 1, 1, 12, 0)]}).with_columns(
            pl.col("a").dt.convert_time_zone("Europe/Moscow"),
            pl.col("n").dt.cast_time_unit("ns"))
        out = self.read(self.extract(df, "--cast-universal"))
        self.assertEqual(out.schema["a"], pl.Datetime("us"))
        self.assertEqual(out.schema["n"], pl.Datetime("us"))
        self.assertEqual(out["a"].to_list()[0], dt.datetime(2024, 1, 1, 9, 0))

    def test_cast_universal_turns_categorical_back_into_strings(self):
        """--cast-universal побеждает --categorize."""
        df = pl.DataFrame({"status": ["a", "b"]})
        out = self.read(self.extract(df, "--categorize", "status", "--cast-universal"))
        self.assertEqual(out.schema["status"], pl.String)

    def test_cast_universal_decimal_scale_zero_becomes_int64(self):
        df = pl.DataFrame({"n": pl.Series("n", [Decimal("42"), None], dtype=pl.Decimal(10, 0))})
        out = self.read(self.extract(df, "--cast-universal"))
        self.assertEqual(out.schema["n"], pl.Int64)
        self.assertEqual(out["n"].to_list(), [42, None])

    def test_decimal_default_is_unchanged_without_cast_universal(self):
        df = pl.DataFrame({"id": [1]}).with_columns(decimal_series("amt", ["10.2500000000"]))
        out = self.read(self.extract(df))
        self.assertEqual(out.schema["amt"], pl.Decimal(38, 10))

    def test_decimal_as_float_without_cast_universal(self):
        df = pl.DataFrame({"id": [1, 2]}).with_columns(decimal_series("amt", ["10.2500000000", None]))
        out = self.read(self.extract(df, "--decimal-as", "float"))
        self.assertEqual(out.schema["amt"], pl.Float64)
        self.assertEqual(out["amt"].to_list(), [10.25, None])

    def test_decimal_as_string_keeps_exact_scale(self):
        df = pl.DataFrame({"id": [1, 2]}).with_columns(
            decimal_series("amt", ["12345678901234567890.1234567891", None]))
        out = self.read(self.extract(df, "--decimal-as", "string"))
        self.assertEqual(out.schema["amt"], pl.String)
        self.assertEqual(out["amt"].to_list(), ["12345678901234567890.1234567891", None])

    def test_decimal_as_decimal_overrides_cast_universal(self):
        df = pl.DataFrame({"id": [1]}).with_columns(decimal_series("amt", ["10.2500000000"]))
        out = self.read(self.extract(df, "--cast-universal", "--decimal-as", "decimal"))
        self.assertEqual(out.schema["amt"], pl.Decimal(38, 10))

    def test_decimal_as_string_with_cast_universal(self):
        df = pl.DataFrame({"id": [1]}).with_columns(decimal_series("amt", ["10.2500000000"]))
        out = self.read(self.extract(df, "--cast-universal", "--decimal-as", "string"))
        self.assertEqual(out["amt"].to_list(), ["10.2500000000"])

    def test_decimal_as_float_scale_zero_becomes_int64(self):
        df = pl.DataFrame({"n": pl.Series("n", [Decimal("42"), None], dtype=pl.Decimal(10, 0))})
        out = self.read(self.extract(df, "--decimal-as", "float"))
        self.assertEqual(out.schema["n"], pl.Int64)

    def test_decimal_as_annotation_overrides_cli(self):
        df = pl.DataFrame({"id": [1]}).with_columns(decimal_series("amt", ["1.5000000000"]))
        out = self.read(self.extract(df, "--decimal-as", "float", sql="-- @DX_DECIMAL_AS: string\nselect 1"))
        self.assertEqual(out["amt"].to_list(), ["1.5000000000"])

    def test_decimal_as_rejects_unknown_value(self):
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.parquet", "--decimal-as", "number"],
                         read=lambda **kw: pl.DataFrame({"a": [1]}))
        self.assertEqual(r.code, 2)

    def test_cast_universal_nested_types_become_json_strings(self):
        df = pl.DataFrame({
            "l": [[1, 2], None, []],
            "s": [{"a": 1, "b": "x"}, None, {"a": None, "b": "y"}],
        })
        out = self.read(self.extract(df, "--cast-universal"))
        self.assertEqual(out.schema["l"], pl.String)
        self.assertEqual(out["l"].to_list(), ["[1,2]", None, "[]"])
        self.assertEqual(out["s"].to_list(), ['{"a":1,"b":"x"}', None, '{"a":null,"b":"y"}'])

    def test_categorize_is_applied_only_to_parquet(self):
        df = pl.DataFrame({"status": ["a", "b", "a"]})
        r = self.run_cli(BASE + ["--query", "select 1", "--out", "o.arrow", "--categorize", "status"],
                         read=lambda **kw: df)
        self.assertEqual(r.code, 0, msg=r.out + r.err + r.log)
        self.assertEqual(pl.read_ipc(r.path("o.arrow")).schema["status"], pl.String)

    def test_csv_gzip_under_plain_csv_name(self):
        df = pl.DataFrame({"a": [1, 2], "b": ["x", "y"]})
        r = self.extract(df, "--compression", "gzip:6", out="plain.csv")
        with gzip.open(r.path("plain.csv"), "rt", encoding="utf-8") as f:
            self.assertEqual(f.read().splitlines(), ["a,b", "1,x", "2,y"])

    def test_csv_gz_extension(self):
        r = self.extract(pl.DataFrame({"a": [1]}), out="data.csv.gz")
        with gzip.open(r.path("data.csv.gz"), "rt", encoding="utf-8") as f:
            self.assertEqual(f.read().splitlines(), ["a", "1"])

    def test_csv_zstd_variants_have_zstd_magic(self):
        for name in ("data.csv.zst", "data.csv.zstd"):
            with self.subTest(name=name):
                r = self.extract(pl.DataFrame({"a": [1]}), out=name)
                with open(r.path(name), "rb") as f:
                    self.assertEqual(f.read(4), b"\x28\xb5\x2f\xfd")

    def test_csv_plain(self):
        r = self.extract(pl.DataFrame({"a": [1]}), out="plain.csv")
        with open(r.path("plain.csv"), encoding="utf-8") as f:
            self.assertEqual(f.read().splitlines(), ["a", "1"])

    def test_jsonl_and_arrow_roundtrip(self):
        df = pl.DataFrame({"a": [1, 2], "b": ["x", None]})
        r = self.extract(df, out="o.jsonl")
        with open(r.path("o.jsonl"), encoding="utf-8") as f:
            rows = [json.loads(line) for line in f]
        self.assertEqual([x["a"] for x in rows], [1, 2])
        r2 = self.extract(df, out="o.arrow", sql="select 2")
        self.assertEqual(pl.read_ipc(r2.path("o.arrow")).to_dicts(), df.to_dicts())

    def test_parquet_compression_codec_is_applied(self):
        df = pl.DataFrame({"a": list(range(100))})
        r = self.extract(df, "--compression", "gzip:3", out="g.parquet")
        self.assertEqual(self.read(r, "g.parquet")["a"].to_list(), list(range(100)))

    def test_uppercase_oracle_style_names_with_annotations(self):
        df = pl.DataFrame({"ID": [1, None], "STATUS": ["a", None], "EMAIL": ["a@x.com", None]})
        sql = "-- @DX_HASH_COLUMNS: email\n-- @DX_FILL_NULLS: status, id\nselect 1"
        out = self.read(self.extract(df, sql=sql))
        self.assertEqual(out["EMAIL"].to_list(), [sha("a@x.com"), None])
        self.assertEqual(out["STATUS"].to_list(), ["a", "null"])
        self.assertEqual(out["ID"].to_list(), [1, 0])

    def test_dtype_selection_works_on_installed_polars(self):
        """fill-nulls не должен зависеть от удалённых в polars 2.0 имён."""
        self.assertTrue(pl.Int64.is_numeric())
        self.assertTrue(pl.Decimal(38, 10).is_numeric())
        self.assertFalse(pl.String.is_numeric())


if __name__ == "__main__":
    unittest.main()
