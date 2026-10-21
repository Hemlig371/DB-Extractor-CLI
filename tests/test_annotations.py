"""extract_dx_params: аннотации `-- @DX_*` и их взаимодействие с аргументами CLI."""
import unittest

from helpers import dbx, ns

extract = dbx.extract_dx_params


class AnnotationParsing(unittest.TestCase):
    """Разбор аннотаций и значения по умолчанию."""

    def test_defaults_come_from_cli(self):
        p = extract("SELECT 1", ns(out="o.csv", format="csv", partition_on="id", partitions=4,
                                  fill_nulls=["a"], drop_nulls=[], utc=True, categorize=["c"],
                                  hash_columns=["h"], cast_universal=True, decimal_as="string"))
        self.assertEqual(p["out"], "o.csv")
        self.assertEqual(p["format"], "csv")
        self.assertEqual((p["partition_on"], p["partitions"]), ("id", 4))
        self.assertEqual(p["fill_nulls"], ["a"])
        self.assertEqual(p["drop_nulls"], [])
        self.assertTrue(p["utc"])
        self.assertEqual(p["categorize"], ["c"])
        self.assertEqual(p["hash_columns"], ["h"])
        self.assertTrue(p["cast_universal"])
        self.assertEqual(p["decimal_as"], "string")

    def test_decimal_as_annotation(self):
        self.assertEqual(extract("-- @DX_DECIMAL_AS: String\nSELECT 1", ns())["decimal_as"], "string")
        self.assertEqual(extract("-- @DX_DECIMAL_AS: float\nSELECT 1", ns(decimal_as="string"))["decimal_as"], "float")
        self.assertIsNone(extract("SELECT 1", ns())["decimal_as"])

    def test_decimal_as_annotation_with_unknown_value_is_ignored(self):
        p = extract("-- @DX_DECIMAL_AS: number\nSELECT 1", ns(decimal_as="float"))
        self.assertEqual(p["decimal_as"], "float")
        self.assertTrue(any("DECIMAL_AS" in w for w in p["warnings"]))

    def test_readme_example_one(self):
        sql = (
            "-- @DX_OUT: reports_2023.parquet\n"
            "-- @DX_PARTITION_ON: report_id\n"
            "-- @DX_PARTITIONS: 4\n"
            "-- @DX_COMPRESSION: zstd:10\n"
            "-- @DX_CATEGORIZE: status, type\n"
            "-- @DX_FILL_NULLS: status, category\n"
            "SELECT * FROM reports WHERE year = 2023"
        )
        p = extract(sql, ns())
        self.assertEqual(p["out"], "reports_2023.parquet")
        self.assertEqual((p["partition_on"], p["partitions"]), ("report_id", 4))
        self.assertEqual((p["compression"], p["compression_level"]), ("zstd", 10))
        self.assertEqual(p["categorize"], ["status", "type"])
        self.assertEqual(p["fill_nulls"], ["status", "category"])

    def test_readme_example_two(self):
        sql = ("-- @DX_OUT: users_active.csv\n-- @DX_COMPRESSION: gzip:6\n"
               "-- @DX_HASH_COLUMNS: email, phone\n-- @DX_UTC\nSELECT * FROM users WHERE active = true")
        p = extract(sql, ns())
        self.assertEqual(p["out"], "users_active.csv")
        self.assertEqual((p["compression"], p["compression_level"]), ("gzip", 6))
        self.assertEqual(p["hash_columns"], ["email", "phone"])
        self.assertTrue(p["utc"])

    def test_annotation_beats_cli(self):
        p = extract("-- @DX_OUT: ann.csv\nSELECT 1", ns(out="cli.csv"))
        self.assertEqual(p["out"], "ann.csv")

    def test_first_duplicate_wins(self):
        p = extract("-- @DX_OUT: a.parquet\n-- @DX_OUT: b.parquet\nSELECT 1", ns())
        self.assertEqual(p["out"], "a.parquet")

    def test_names_are_case_insensitive_and_format_is_lowercased(self):
        p = extract("-- @dx_out: A.CSV\n-- @DX_Format: CSV\nSELECT 1", ns())
        self.assertEqual((p["out"], p["format"]), ("A.CSV", "csv"))

    def test_marker_spacing_variants(self):
        self.assertEqual(extract("--@DX_OUT: z.csv\nSELECT 1", ns())["out"], "z.csv")
        self.assertEqual(extract("--\t@DX_OUT: t.csv\nSELECT 1", ns())["out"], "t.csv")

    def test_fill_nulls_with_list_and_without_colon(self):
        self.assertEqual(extract("-- @DX_FILL_NULLS: a, b\nSELECT 1", ns())["fill_nulls"], ["a", "b"])
        self.assertEqual(extract("-- @DX_FILL_NULLS a, b\nSELECT 1", ns())["fill_nulls"], ["a", "b"])

    def test_cli_compression_with_level(self):
        p = extract("SELECT 1", ns(compression="zstd:10"))
        self.assertEqual((p["compression"], p["compression_level"]), ("zstd", 10))

    def test_gz_alias(self):
        p = extract("SELECT 1", ns(compression="gz:6"))
        self.assertEqual((p["compression"], p["compression_level"]), ("gzip", 6))

    def test_annotation_level_overrides_cli_level(self):
        p = extract("-- @DX_COMPRESSION: zstd:3\nSELECT 1", ns(compression="zstd:10"))
        self.assertEqual((p["compression"], p["compression_level"]), ("zstd", 3))

    def test_annotation_same_algorithm_keeps_cli_level(self):
        p = extract("-- @DX_COMPRESSION: zstd\nSELECT 1", ns(compression="zstd:10"))
        self.assertEqual((p["compression"], p["compression_level"]), ("zstd", 10))

    def test_annotation_after_semicolon_belongs_to_next_query(self):
        first, second = dbx.split_sql_queries("select 1; -- @DX_OUT: b.parquet\nselect 2")
        self.assertIsNone(extract(first, ns())["out"])
        self.assertEqual(extract(second, ns())["out"], "b.parquet")


class AnnotationValues(unittest.TestCase):
    """Захват значения, нормализация, привязка аннотаций к комментариям."""

    def test_bare_fill_nulls_means_all_columns(self):
        p = extract("-- @DX_FILL_NULLS\nSELECT a, b FROM t", ns())
        self.assertEqual(p["fill_nulls"], [])

    def test_bare_drop_nulls_means_all_columns(self):
        p = extract("-- @DX_DROP_NULLS\nSELECT a FROM t", ns())
        self.assertEqual(p["drop_nulls"], [])

    def test_bare_fill_nulls_does_not_swallow_next_annotation(self):
        p = extract("-- @DX_FILL_NULLS\n-- @DX_UTC\nSELECT a FROM t", ns())
        self.assertEqual(p["fill_nulls"], [])
        self.assertTrue(p["utc"])

    def test_fill_nulls_at_end_of_line_after_code(self):
        p = extract("SELECT a FROM t -- @DX_FILL_NULLS", ns())
        self.assertEqual(p["fill_nulls"], [])

    def test_fill_nulls_space_separated(self):
        self.assertEqual(extract("-- @DX_FILL_NULLS: a b\nSELECT 1", ns())["fill_nulls"], ["a", "b"])

    def test_empty_values_are_ignored_not_taken_from_next_line(self):
        for name in ("OUT", "FORMAT", "COMPRESSION", "PARTITION_ON", "CATEGORIZE", "HASH_COLUMNS", "PARTITIONS"):
            with self.subTest(name=name):
                p = extract("-- @DX_%s:\nSELECT a FROM t" % name, ns())
                self.assertIsNone(p["out"])
                self.assertIsNone(p["format"])
                self.assertIsNone(p["compression"])
                self.assertIsNone(p["partition_on"])
                self.assertIsNone(p["partitions"])
                self.assertIsNone(p["categorize"])
                self.assertIsNone(p["hash_columns"])
                self.assertTrue(p["warnings"], "ignored empty value must be reported")

    def test_out_path_with_spaces(self):
        p = extract("-- @DX_OUT: C:\\Users\\me\\my data\\a.parquet\nSELECT 1", ns())
        self.assertEqual(p["out"], "C:\\Users\\me\\my data\\a.parquet")

    def test_out_path_with_crlf(self):
        p = extract("-- @DX_OUT: a b.parquet\r\nSELECT 1", ns())
        self.assertEqual(p["out"], "a b.parquet")

    def test_annotation_inside_string_literal_is_ignored(self):
        p = extract("SELECT '-- @DX_OUT: evil.parquet' AS x FROM t", ns())
        self.assertIsNone(p["out"])

    def test_annotation_name_must_end_at_word_boundary(self):
        p = extract("-- @DX_UTC_FOO\n-- @DX_CAST_UNIVERSAL_X\nSELECT 1", ns())
        self.assertFalse(p["utc"])
        self.assertFalse(p["cast_universal"])

    def test_annotation_inside_block_comment_still_works(self):
        p = extract("/*\n-- @DX_OUT: m.csv\n*/ SELECT 1", ns())
        self.assertEqual(p["out"], "m.csv")

    def test_compression_is_always_lowercased(self):
        for cli, expected in (("ZSTD", "zstd"), ("GZ", "gzip"), ("GZIP", "gzip"), ("Zstd:5", "zstd")):
            with self.subTest(cli=cli):
                self.assertEqual(extract("SELECT 1", ns(compression=cli))["compression"], expected)

    def test_annotation_compression_lowercased(self):
        p = extract("-- @DX_COMPRESSION: ZSTD:10\nSELECT 1", ns())
        self.assertEqual((p["compression"], p["compression_level"]), ("zstd", 10))

    def test_cli_level_does_not_leak_into_other_algorithm(self):
        p = extract("-- @DX_COMPRESSION: gzip\nSELECT 1", ns(compression="zstd:10"))
        self.assertEqual((p["compression"], p["compression_level"]), ("gzip", None))

    def test_invalid_level_is_ignored_with_warning(self):
        p = extract("SELECT 1", ns(compression="zstd:abc"))
        self.assertEqual((p["compression"], p["compression_level"]), ("zstd", None))
        self.assertTrue(p["warnings"])


if __name__ == "__main__":
    unittest.main()
