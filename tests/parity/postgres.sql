DROP TABLE IF EXISTS parity_types;
CREATE TABLE parity_types (
    id int PRIMARY KEY, i2 smallint, i8 bigint, r4 real, r8 double precision,
    n12_3 numeric(12,3), n10_0 numeric(10,0),
    txt text, vc varchar(20), ch char(5), b boolean,
    dt date, ts timestamp, tstz timestamptz, tm time,
    js json, jsb jsonb, ba bytea, u uuid, ip inet,
    arr int[], tarr text[], iv interval, mon money
);
INSERT INTO parity_types VALUES
 (1, 32767, 9223372036854775807, 1.5, 2.25, 12345.678, 42,
  'hello', 'v1', 'ab', true, '2024-01-02', '2024-01-02 03:04:05.123456', '2024-01-02 03:04:05.123456+03', '10:11:12.5',
  '{"a":1}', '{"b":[1,2]}', '\xdeadbeef', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '10.0.0.1',
  '{1,2,3}', '{"x","y z"}', '1 day', '$12.34'),
 (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
  NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
 (3, -1, -5, -0.5, 1e300, -0.001, -7,
  E'юникод, "кавычки"\nперевод', 'x', 'q', false, '1969-12-31', '1969-12-31 23:59:59', '1970-01-01 00:00:00+00', '00:00:00',
  '[]', 'null', '', '00000000-0000-0000-0000-000000000000', '::1',
  '{}', '{}', '00:00:01', '$0.00');

DROP TABLE IF EXISTS parity_big;
CREATE TABLE parity_big AS
SELECT g AS id, md5(g::text) AS h, (g % 1000)::int AS k, g * 1.5::float8 AS f,
       CASE WHEN g % 7 = 0 THEN NULL ELSE 'row ' || g END AS s
FROM generate_series(1, 20000) g;
