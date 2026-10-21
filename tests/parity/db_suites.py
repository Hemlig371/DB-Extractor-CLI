"""Сценарии сравнения Python и Rust на настоящих СУБД (PostgreSQL, Oracle) и проверки Rust на ClickHouse.

Таблицы создают postgres.sql, oracle.sql и clickhouse.sql. Сценарий с rust_only запускается только в Rust-сборке и
сверяется с ожидаемым текстом: connectorx из PyPI (Python-версия) эти типы Oracle не читает.
"""


def sc(name, args, outs, kind, sql, exit_code=0, sort=False, query_file=None, rust_only=False, expect=None):
    return dict(name=name, args=args, outs=outs, kind=kind, sql=sql, exit_code=exit_code, sort=sort,
                query_file=query_file, rust_only=rust_only, expect=expect)


PG_NUMBERS = "select id, i2, i8, r4, r8, n12_3, n10_0 from parity_types order by id"
PG_TEXT = "select id, txt, vc, ch, b from parity_types order by id"
PG_TIME = "select id, dt, ts, tstz, tm from parity_types order by id"
PG_OTHER = "select id, js, jsb, ba, u, ip from parity_types order by id"
PG_NESTED = "select id, arr, tarr from parity_types order by id"
PG_ALL = ("select id, i2, i8, r4, r8, n12_3, n10_0, txt, vc, ch, b, dt, ts, tstz, tm, js, jsb, ba, u, ip "
          "from parity_types order by id")
PG_BIG = "select id, h, k, f, s from parity_big order by id"

POSTGRES = [
    sc("pg numbers parquet", [], ["o.parquet"], "parquet", PG_NUMBERS),
    sc("pg numbers csv", [], ["o.csv"], "csv", PG_NUMBERS),
    sc("pg numbers jsonl", [], ["o.jsonl"], "jsonl", PG_NUMBERS),
    sc("pg text jsonl", [], ["o.jsonl"], "jsonl", PG_TEXT),
    sc("pg json, bytea, uuid, inet jsonl", [], ["o.jsonl"], "jsonl", PG_OTHER),
    sc("pg all types csv", [], ["o.csv"], "csv", PG_ALL),
    sc("pg all types jsonl", [], ["o.jsonl"], "jsonl", PG_ALL),
    sc("pg utc jsonl", ["--utc"], ["o.jsonl"], "jsonl", PG_TIME),
    sc("pg all types cast-universal jsonl", ["--cast-universal"], ["o.jsonl"], "jsonl", PG_ALL),
    sc("pg decimal-as float parquet", ["--decimal-as", "float"], ["o.parquet"], "parquet", PG_NUMBERS),
    sc("pg decimal-as string parquet", ["--decimal-as", "string"], ["o.parquet"], "parquet", PG_NUMBERS),
    sc("pg decimal-as string csv", ["--decimal-as", "string"], ["o.csv"], "csv", PG_NUMBERS),
    sc("pg decimal-as string jsonl", ["--decimal-as", "string"], ["o.jsonl"], "jsonl", PG_NUMBERS),
    sc("pg decimal-as decimal with cast-universal", ["--cast-universal", "--decimal-as", "decimal"],
       ["o.parquet"], "parquet", PG_NUMBERS),
    sc("pg decimal-as string with cast-universal", ["--cast-universal", "--decimal-as", "string"],
       ["o.jsonl"], "jsonl", PG_ALL),
    sc("pg hash decimal column with decimal-as string", ["--hash-columns", "n12_3", "--decimal-as", "string"],
       ["o.parquet"], "parquet", PG_NUMBERS),
    sc("pg text parquet", [], ["o.parquet"], "parquet", PG_TEXT),
    sc("pg text csv", [], ["o.csv"], "csv", PG_TEXT),
    sc("pg date and time parquet", [], ["o.parquet"], "parquet", PG_TIME),
    sc("pg date and time csv", [], ["o.csv"], "csv", PG_TIME),
    sc("pg date and time jsonl", [], ["o.jsonl"], "jsonl", PG_TIME),
    sc("pg json, bytea, uuid, inet parquet", [], ["o.parquet"], "parquet", PG_OTHER),
    sc("pg json, bytea, uuid, inet csv", [], ["o.csv"], "csv", PG_OTHER),
    sc("pg arrays parquet", [], ["o.parquet"], "parquet", PG_NESTED),
    sc("pg arrays cast-universal jsonl", ["--cast-universal"], ["o.jsonl"], "jsonl", PG_NESTED),
    sc("pg all types cast-universal parquet", ["--cast-universal"], ["o.parquet"], "parquet", PG_ALL),
    sc("pg all types cast-universal csv", ["--cast-universal"], ["o.csv"], "csv", PG_ALL),
    sc("pg utc parquet", ["--utc"], ["o.parquet"], "parquet", PG_TIME),
    sc("pg utc csv", ["--utc"], ["o.csv"], "csv", PG_TIME),
    sc("pg hash text and int", ["--hash-columns", "txt", "id"], ["o.parquet"], "parquet", PG_TEXT),
    sc("pg hash with salt", ["--hash-columns", "txt", "--hash-salt", "s3cret"], ["o.csv"], "csv", PG_TEXT),
    sc("pg fill-nulls", ["--fill-nulls"], ["o.parquet"], "parquet", PG_TEXT),
    sc("pg drop-nulls", ["--drop-nulls", "txt"], ["o.csv"], "csv", PG_TEXT),
    sc("pg categorize", ["--categorize", "vc"], ["o.parquet"], "parquet", PG_TEXT),
    sc("pg big parquet", [], ["o.parquet"], "parquet", PG_BIG),
    sc("pg big csv", [], ["o.csv"], "csv", PG_BIG),
    sc("pg big partitions", ["--partition-on", "id", "--partitions", "3"], ["o.parquet"], "parquet", PG_BIG,
       sort=True),
    sc("pg empty result", [], ["o.csv"], "csv", "select id, txt from parity_types where 1 = 0"),
    sc("pg unsupported interval fails alike", [], [], "parquet", "select id, iv from parity_types", exit_code=1),
    sc("pg unsupported money fails alike", [], [], "parquet", "select id, mon from parity_types", exit_code=1),
    sc("pg broken sql fails", [], [], "parquet", "select * from no_such_table", exit_code=1),
    sc("pg annotations via query file", [], ["a.parquet", "b.csv"], None, None,
       query_file=(
           "-- @DX_OUT: a.parquet\n-- @DX_HASH_COLUMNS: txt\n-- @DX_FILL_NULLS\n" + PG_TEXT + ";\n"
           "-- @DX_OUT: b.csv\n-- @DX_DROP_NULLS: txt\n" + PG_TEXT + ";\n"
       )),
]

OR_NUMBERS = "select id, n_int, n_dec, n_any, bd, bf from parity_types order by id"
OR_TEXT = "select id, vc, nvc, ch, cl from parity_types order by id"
OR_TIME = "select id, d, ts from parity_types order by id"
OR_TZ = "select id, tstz from parity_types order by id"
OR_BINARY = "select id, bl from parity_types order by id"
OR_ALL = "select id, n_int, n_dec, n_any, bd, bf, vc, nvc, ch, cl, bl, d, ts from parity_types order by id"
OR_BIG = "select id, h, k, f, s from parity_big order by id"
OR_NEW = "select id, flag, rw, nc, ids, iym, ltz from parity_new order by id"

ORACLE = [
    sc("oracle numbers parquet", [], ["o.parquet"], "parquet", OR_NUMBERS),
    sc("oracle numbers csv", [], ["o.csv"], "csv", OR_NUMBERS),
    sc("oracle numbers jsonl", [], ["o.jsonl"], "jsonl", OR_NUMBERS),
    sc("oracle text jsonl", [], ["o.jsonl"], "jsonl", OR_TEXT),
    sc("oracle date and time jsonl", [], ["o.jsonl"], "jsonl", OR_TIME),
    sc("oracle blob csv", [], ["o.csv"], "csv", OR_BINARY),
    sc("oracle blob jsonl", [], ["o.jsonl"], "jsonl", OR_BINARY),
    sc("oracle all cast-universal jsonl", ["--cast-universal"], ["o.jsonl"], "jsonl", OR_ALL),
    sc("oracle decimal-as float parquet", ["--decimal-as", "float"], ["o.parquet"], "parquet", OR_NUMBERS),
    sc("oracle decimal-as string parquet", ["--decimal-as", "string"], ["o.parquet"], "parquet", OR_NUMBERS),
    sc("oracle decimal-as string csv", ["--decimal-as", "string"], ["o.csv"], "csv", OR_NUMBERS),
    sc("oracle decimal-as string jsonl", ["--decimal-as", "string"], ["o.jsonl"], "jsonl", OR_NUMBERS),
    sc("oracle text parquet", [], ["o.parquet"], "parquet", OR_TEXT),
    sc("oracle text csv", [], ["o.csv"], "csv", OR_TEXT),
    sc("oracle date and time parquet", [], ["o.parquet"], "parquet", OR_TIME),
    sc("oracle date and time csv", [], ["o.csv"], "csv", OR_TIME),
    sc("oracle blob parquet", [], ["o.parquet"], "parquet", OR_BINARY),
    sc("oracle all cast-universal parquet", ["--cast-universal"], ["o.parquet"], "parquet", OR_ALL),
    sc("oracle all cast-universal csv", ["--cast-universal"], ["o.csv"], "csv", OR_ALL),
    sc("oracle utc csv", ["--utc"], ["o.csv"], "csv", OR_TIME),
    sc("oracle hash column given in lowercase", ["--hash-columns", "vc"], ["o.parquet"], "parquet",
       OR_TEXT),
    sc("oracle fill-nulls", ["--fill-nulls"], ["o.parquet"], "parquet", OR_TEXT),
    sc("oracle drop-nulls", ["--drop-nulls", "vc"], ["o.csv"], "csv", OR_TEXT),
    sc("oracle big parquet", [], ["o.parquet"], "parquet", OR_BIG),
    sc("oracle big partitions", ["--partition-on", "id", "--partitions", "3"], ["o.parquet"], "parquet", OR_BIG,
       sort=True),
    sc("oracle empty result", [], ["o.csv"], "csv", "select id, vc from parity_types where 1 = 0"),
    sc("oracle broken sql fails", [], [], "parquet", "select * from no_such_table", exit_code=1),
    # типы, которые читает только патч connectorx в Rust-сборке
    sc("oracle patched types csv", [], ["o.csv"], "csv", OR_NEW, rust_only=True, expect={"o.csv": [
        ["ID", "FLAG", "RW", "NC", "IDS", "IYM", "LTZ"],
        ["1", "true", "cafebabe", "nclob text", "+01 02:03:04.500", "+03-04", "2024-01-02T03:04:05.123456+0000"],
        ["2", "false", "00", "", "", "", ""],
        ["3", "", "", "", "", "", ""],
    ]}),
    sc("oracle timestamp with time zone csv", [], ["o.csv"], "csv", OR_TZ, rust_only=True, expect={"o.csv": [
        ["ID", "TSTZ"],
        ["1", "2024-01-02T00:04:05.123456+0000"],
        ["2", ""],
        ["3", "1970-01-01T00:00:00.000000+0000"],
    ]}),
    sc("oracle timestamp with time zone jsonl", [], ["o.jsonl"], "jsonl", OR_TZ, rust_only=True, expect={"o.jsonl": [
        {"ID": 1, "TSTZ": "2024-01-02T00:04:05.123456+00:00"},
        {"ID": 2, "TSTZ": None},
        {"ID": 3, "TSTZ": "1970-01-01T00:00:00+00:00"},
    ]}),
    sc("oracle rowid csv", [], ["o.csv"], "csv", "select id, rowid as rid from parity_new order by id",
       rust_only=True, expect={"o.csv": [
           ["ID", "RID"], ["1", "re:^[A-Za-z0-9+/]{18}$"], ["2", "re:^[A-Za-z0-9+/]{18}$"],
           ["3", "re:^[A-Za-z0-9+/]{18}$"]]}),
    sc("oracle patched types cast-universal jsonl", ["--cast-universal"], ["o.jsonl"], "jsonl", OR_NEW,
       rust_only=True, expect={"o.jsonl": [
           {"ID": 1, "FLAG": True, "RW": "cafebabe", "NC": "nclob text", "IDS": "+01 02:03:04.500", "IYM": "+03-04",
            "LTZ": "2024-01-02 03:04:05.123456"},
           {"ID": 2, "FLAG": False, "RW": "00", "NC": None, "IDS": None, "IYM": None, "LTZ": None},
           {"ID": 3, "FLAG": None, "RW": None, "NC": None, "IDS": None, "IYM": None, "LTZ": None},
       ]}),
]

CH_NUMBERS = "select id, i8c, u8c, i64, u64, f32, f64, d12_3, d10_0 from parity_types order by id"
CH_TEXT = "select id, s, fs, e, u, ip4, b from parity_types order by id"
CH_TIME = "select id, dt, dt32, ts, ts3, ts6 from parity_types order by id"
CH_NESTED = "select id, arr, sarr from parity_types order by id"
CH_ALL = ("select id, i8c, u8c, i64, u64, f32, f64, d12_3, d10_0, s, fs, b, dt, dt32, ts, ts3, ts6, e, u, ip4, arr, sarr "
          "from parity_types order by id")
CH_BIG = "select id, h, k, f, s from parity_big order by id"

CLICKHOUSE = [
    sc("ch numbers parquet", [], ["o.parquet"], "parquet", CH_NUMBERS),
    sc("ch numbers csv", [], ["o.csv"], "csv", CH_NUMBERS),
    sc("ch numbers jsonl", [], ["o.jsonl"], "jsonl", CH_NUMBERS),
    sc("ch text parquet", [], ["o.parquet"], "parquet", CH_TEXT),
    sc("ch text csv", [], ["o.csv"], "csv", CH_TEXT),
    sc("ch text jsonl", [], ["o.jsonl"], "jsonl", CH_TEXT),
    sc("ch date and time parquet", [], ["o.parquet"], "parquet", CH_TIME),
    sc("ch date and time csv", [], ["o.csv"], "csv", CH_TIME),
    sc("ch date and time jsonl", [], ["o.jsonl"], "jsonl", CH_TIME),
    sc("ch arrays parquet", [], ["o.parquet"], "parquet", CH_NESTED),
    sc("ch arrays cast-universal jsonl", ["--cast-universal"], ["o.jsonl"], "jsonl", CH_NESTED),
    sc("ch all types parquet", [], ["o.parquet"], "parquet", CH_ALL),
    sc("ch all types jsonl", [], ["o.jsonl"], "jsonl", CH_ALL),
    sc("ch all types cast-universal parquet", ["--cast-universal"], ["o.parquet"], "parquet", CH_ALL),
    sc("ch all types cast-universal csv", ["--cast-universal"], ["o.csv"], "csv", CH_ALL),
    sc("ch all types cast-universal jsonl", ["--cast-universal"], ["o.jsonl"], "jsonl", CH_ALL),
    sc("ch decimal-as float parquet", ["--decimal-as", "float"], ["o.parquet"], "parquet", CH_NUMBERS),
    sc("ch decimal-as string parquet", ["--decimal-as", "string"], ["o.parquet"], "parquet", CH_NUMBERS),
    sc("ch decimal-as string csv", ["--decimal-as", "string"], ["o.csv"], "csv", CH_NUMBERS),
    sc("ch decimal-as string jsonl", ["--decimal-as", "string"], ["o.jsonl"], "jsonl", CH_NUMBERS),
    sc("ch decimal-as string exact csv", ["--decimal-as", "string"], ["o.csv"], "csv",
       "select id, d12_3, d10_0 from parity_types order by id", expect={"o.csv": [
           ["id", "d12_3", "d10_0"],
           ["1", "12345.6780000000", "42.0000000000"],
           ["2", "", ""],
           ["3", "-0.0010000000", "-7.0000000000"]]}),
    sc("ch decimal-as string exact jsonl", ["--decimal-as", "string"], ["o.jsonl"], "jsonl",
       "select id, d12_3, d10_0 from parity_types order by id", expect={"o.jsonl": [
           {"id": 1, "d12_3": "12345.6780000000", "d10_0": "42.0000000000"},
           {"id": 2, "d12_3": None, "d10_0": None},
           {"id": 3, "d12_3": "-0.0010000000", "d10_0": "-7.0000000000"}]}),
    sc("ch decimal-as float exact jsonl", ["--decimal-as", "float"], ["o.jsonl"], "jsonl",
       "select id, d12_3, d10_0 from parity_types order by id", expect={"o.jsonl": [
           {"id": 1, "d12_3": 12345.678, "d10_0": 42.0},
           {"id": 2, "d12_3": None, "d10_0": None},
           {"id": 3, "d12_3": -0.001, "d10_0": -7.0}]}),
    sc("ch decimal-as decimal with cast-universal", ["--cast-universal", "--decimal-as", "decimal"],
       ["o.parquet"], "parquet", CH_NUMBERS),
    sc("ch utc parquet", ["--utc"], ["o.parquet"], "parquet", CH_TIME),
    sc("ch utc csv", ["--utc"], ["o.csv"], "csv", CH_TIME),
    sc("ch hash text and int", ["--hash-columns", "s", "id"], ["o.parquet"], "parquet", CH_TEXT),
    sc("ch hash with salt", ["--hash-columns", "s", "--hash-salt", "s3cret"], ["o.csv"], "csv", CH_TEXT),
    sc("ch fill-nulls", ["--fill-nulls"], ["o.parquet"], "parquet", CH_TEXT),
    sc("ch drop-nulls", ["--drop-nulls", "s"], ["o.csv"], "csv", CH_TEXT),
    sc("ch categorize", ["--categorize", "s"], ["o.parquet"], "parquet", CH_TEXT),
    sc("ch big parquet", [], ["o.parquet"], "parquet", CH_BIG),
    sc("ch big csv", [], ["o.csv"], "csv", CH_BIG),
    sc("ch big partitions", ["--partition-on", "id", "--partitions", "3"], ["o.parquet"], "parquet", CH_BIG,
       sort=True),
    sc("ch empty result", [], ["o.csv"], "csv", "select id, s from parity_types where 1 = 0"),
    sc("ch broken sql fails", [], [], "parquet", "select * from no_such_table", exit_code=1),
    sc("ch annotations via query file", [], ["a.parquet", "b.csv"], None, None,
       query_file=(
           "-- @DX_OUT: a.parquet\n-- @DX_HASH_COLUMNS: s\n-- @DX_FILL_NULLS\n" + CH_TEXT + ";\n"
           "-- @DX_OUT: b.csv\n-- @DX_DECIMAL_AS: string\n" + CH_NUMBERS + ";\n"
       )),
]

SUITES = {"postgres": POSTGRES, "oracle": ORACLE, "clickhouse": CLICKHOUSE}
