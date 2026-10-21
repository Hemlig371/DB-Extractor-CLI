"""split_sql_queries: разбиение SQL по `;` с учётом строк и комментариев."""
import unittest

from helpers import dbx

split = dbx.split_sql_queries


class SplitCharacterization(unittest.TestCase):
    """Поведение, которое должно сохраниться без изменений."""

    def check(self, sql, expected):
        self.assertEqual(split(sql), expected, msg=repr(sql))

    def test_basic(self):
        self.check("select 1; select 2;", ["select 1", "select 2"])

    def test_no_trailing_semicolon(self):
        self.check("select 1; select 2", ["select 1", "select 2"])

    def test_empty_and_only_separators(self):
        self.check("", [])
        self.check("   \n ", [])
        self.check(";;;", [])

    def test_semicolon_in_single_quotes(self):
        self.check("select 'a;b' from t; select 2", ["select 'a;b' from t", "select 2"])

    def test_doubled_quote_escape(self):
        self.check("select 'it''s;x' from t; select 2", ["select 'it''s;x' from t", "select 2"])

    def test_semicolon_in_double_quoted_identifier(self):
        self.check('select 1 as "a;b" from t; select 2', ['select 1 as "a;b" from t', "select 2"])

    def test_semicolon_in_line_comment(self):
        self.check("select 1 -- a;b\n, 2; select 3", ["select 1 -- a;b\n, 2", "select 3"])

    def test_semicolon_in_block_comment(self):
        self.check("select 1 /* ; */, 2; select 3", ["select 1 /* ; */, 2", "select 3"])

    def test_dashes_inside_string_are_not_a_comment(self):
        self.check("select 'a -- b; c' from t; select 2", ["select 'a -- b; c' from t", "select 2"])

    def test_leading_annotations_stay_with_their_query(self):
        self.check(
            "-- @DX_OUT: a.parquet\nselect 1; -- @DX_OUT: b.parquet\nselect 2",
            ["-- @DX_OUT: a.parquet\nselect 1", "-- @DX_OUT: b.parquet\nselect 2"],
        )

    def test_comments_are_preserved_in_query_text(self):
        self.check("/* c */ select 1", ["/* c */ select 1"])


class SplitFixes(unittest.TestCase):
    """Дефекты 6: комментарий-хвост и диалектные литералы."""

    def check(self, sql, expected):
        self.assertEqual(split(sql), expected, msg=repr(sql))

    def test_trailing_comment_is_not_a_query(self):
        self.check("select 1;\n-- конец файла\n", ["select 1"])

    def test_trailing_block_comment_is_not_a_query(self):
        self.check("select 1;\n/* x */", ["select 1"])

    def test_comment_only_input(self):
        self.check("-- nothing here\n/* nor here */", [])

    def test_comment_between_queries_is_not_a_query(self):
        self.check("select 1;\n-- middle\n;select 2", ["select 1", "select 2"])

    def test_dollar_quoted_string(self):
        self.check("select $$a;b$$; select 2", ["select $$a;b$$", "select 2"])

    def test_tagged_dollar_quoted_string(self):
        self.check("select $fn$a;b$fn$; select 2", ["select $fn$a;b$fn$", "select 2"])

    def test_dollar_in_identifier_is_not_a_quote(self):
        self.check("select * from v$session; select 2", ["select * from v$session", "select 2"])

    def test_positional_parameter_is_not_a_quote(self):
        self.check("select $1; select 2", ["select $1", "select 2"])

    def test_backtick_identifier(self):
        self.check("select `a;b` from t; select 2", ["select `a;b` from t", "select 2"])

    def test_oracle_q_literal_with_apostrophe(self):
        self.check("select q'[it's; x]' from dual; select 2", ["select q'[it's; x]' from dual", "select 2"])

    def test_oracle_q_literal_other_delimiters(self):
        self.check("select q'{a;b}' from dual; select 2", ["select q'{a;b}' from dual", "select 2"])
        self.check("select q'!a;b!' from dual; select 2", ["select q'!a;b!' from dual", "select 2"])
        self.check("select nq'[a;b]' from dual; select 2", ["select nq'[a;b]' from dual", "select 2"])

    def test_identifier_ending_with_q_is_not_a_q_literal(self):
        self.check("select seq'a;b' from t; select 2", ["select seq'a;b' from t", "select 2"])


class StripComments(unittest.TestCase):
    def test_strip_comments_keeps_code_and_strings(self):
        self.assertEqual(
            " ".join(dbx.strip_sql_comments("-- @DX_OUT: a\nselect 'x -- y' /* c */ from t").split()),
            "select 'x -- y' from t",
        )


if __name__ == "__main__":
    unittest.main()
