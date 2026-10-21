DROP TABLE IF EXISTS parity_types;

CREATE TABLE parity_types (
    id Int32,
    i8c Nullable(Int8), u8c Nullable(UInt8), i64 Nullable(Int64), u64 Nullable(UInt64),
    f32 Nullable(Float32), f64 Nullable(Float64),
    d12_3 Nullable(Decimal(12, 3)), d10_0 Nullable(Decimal(10, 0)),
    s Nullable(String), fs Nullable(FixedString(5)), b Nullable(Bool),
    dt Nullable(Date), dt32 Nullable(Date32),
    ts Nullable(DateTime('UTC')), ts3 Nullable(DateTime64(3, 'UTC')), ts6 Nullable(DateTime64(6, 'UTC')),
    e Nullable(Enum8('a' = 1, 'b' = 2)), u Nullable(UUID), ip4 Nullable(IPv4),
    arr Array(Int32), sarr Array(String)
) ENGINE = MergeTree ORDER BY id;

INSERT INTO parity_types VALUES
 (1, 127, 255, 4611686018427387904, 4611686018427387904, 1.5, 2.25, 12345.678, 42,
  'hello', 'ab', true, '2024-01-02', '2024-01-02', '2024-01-02 03:04:05', '2024-01-02 03:04:05.123',
  '2024-01-02 03:04:05.123456', 'a', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '10.0.0.1', [1, 2, 3], ['x', 'y z']),
 (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
  NULL, NULL, NULL, [], []),
 (3, -128, 0, -5, 0, -0.5, 1e300, -0.001, -7,
  'юникод, "кавычки"\nперевод', 'q', false, '1970-01-01', '1900-01-01', '1970-01-01 00:00:00',
  '1970-01-01 00:00:00.000', '1970-01-01 00:00:00.000000', 'b', '00000000-0000-0000-0000-000000000000',
  '127.0.0.1', [], []);

DROP TABLE IF EXISTS parity_big;

CREATE TABLE parity_big ENGINE = MergeTree ORDER BY id AS
SELECT toInt32(number + 1) AS id, lower(hex(MD5(toString(number + 1)))) AS h, toInt32((number + 1) % 1000) AS k,
       (number + 1) * 1.5 AS f,
       if((number + 1) % 7 = 0, NULL, concat('row ', toString(number + 1))) AS s
FROM numbers(20000);

DROP TABLE IF EXISTS rt_float;
CREATE TABLE rt_float (id Int32, d12_3 Nullable(Float64), d10_0 Nullable(Float64)) ENGINE = MergeTree ORDER BY id;

DROP TABLE IF EXISTS rt_string;
CREATE TABLE rt_string (id Int32, d12_3 Nullable(String), d10_0 Nullable(String)) ENGINE = MergeTree ORDER BY id;

DROP TABLE IF EXISTS rt_decimal;
CREATE TABLE rt_decimal (id Int32, d12_3 Nullable(Decimal(38, 10)), d10_0 Nullable(Decimal(38, 10))) ENGINE = MergeTree ORDER BY id;
