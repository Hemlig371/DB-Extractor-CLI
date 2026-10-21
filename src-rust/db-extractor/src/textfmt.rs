//! Запись пакетов в CSV и JSONL в формате polars: файлы Python- и Rust-сборок совпадают побайтно.
//! Писатели arrow не годятся: пустая строка в них неотличима от NULL, а числа с плавающей точкой,
//! время и decimal выглядят иначе.

use std::fmt::Display;
use std::io::Write;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray};
use arrow::compute::cast;
use arrow::datatypes::{
    DataType, Date32Type, Decimal128Type, Float16Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type,
    Int8Type, Schema, Time32MillisecondType, Time32SecondType, Time64MicrosecondType, Time64NanosecondType, TimeUnit,
    TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType, TimestampSecondType, UInt16Type,
    UInt32Type, UInt64Type, UInt8Type,
};
use arrow::record_batch::RecordBatch;
use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use dbx_core::textfmt::{push_csv_str, push_decimal, push_hex, push_json_str};

use crate::transform::to_json_strings;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Csv,
    Json,
    /// Текст значения, как у приведения к строке в polars (для `--hash-columns`).
    Text,
}

type Cell<'a> = Box<dyn Fn(usize, &mut Vec<u8>) + 'a>;

/// Дней от 0001-01-01 (CE) до 1970-01-01.
const UNIX_EPOCH_DAYS_FROM_CE: i32 = 719_163;

fn quoted(mode: Mode) -> bool {
    mode == Mode::Json
}

fn put_quoted(b: &mut Vec<u8>, mode: Mode, text: impl FnOnce(&mut Vec<u8>)) {
    if quoted(mode) {
        b.push(b'"');
    }
    text(b);
    if quoted(mode) {
        b.push(b'"');
    }
}

fn items(format: &'static str) -> Vec<Item<'static>> {
    StrftimeItems::new(format).parse().expect("static strftime format")
}

fn push_f64(b: &mut Vec<u8>, v: f64, mode: Mode) {
    if mode == Mode::Json && !v.is_finite() {
        b.extend_from_slice(b"null");
        return;
    }
    let mut z = zmij::Buffer::new();
    b.extend_from_slice(z.format(v).as_bytes());
}

fn push_f32(b: &mut Vec<u8>, v: f32, mode: Mode) {
    if mode == Mode::Json && !v.is_finite() {
        b.extend_from_slice(b"null");
        return;
    }
    let mut z = zmij::Buffer::new();
    b.extend_from_slice(z.format(v).as_bytes());
}

fn split_timestamp(v: i64, unit: &TimeUnit) -> (i64, u32) {
    match unit {
        TimeUnit::Second => (v, 0),
        TimeUnit::Millisecond => (v.div_euclid(1_000), (v.rem_euclid(1_000) * 1_000_000) as u32),
        TimeUnit::Microsecond => (v.div_euclid(1_000_000), (v.rem_euclid(1_000_000) * 1_000) as u32),
        TimeUnit::Nanosecond => (v.div_euclid(1_000_000_000), v.rem_euclid(1_000_000_000) as u32),
    }
}

fn naive_datetime(v: i64, unit: &TimeUnit) -> Option<NaiveDateTime> {
    let (secs, nanos) = split_timestamp(v, unit);
    DateTime::from_timestamp(secs, nanos).map(|d| d.naive_utc())
}

enum Zone {
    Fixed(FixedOffset),
    Named(chrono_tz::Tz),
}

fn parse_offset(s: &str) -> Option<FixedOffset> {
    let sign = match *s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &s[1..];
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

fn parse_zone(s: &str) -> Result<Zone, String> {
    if let Some(off) = parse_offset(s) {
        return Ok(Zone::Fixed(off));
    }
    s.parse::<chrono_tz::Tz>()
        .map(Zone::Named)
        .map_err(|_| format!("unknown time zone '{}'", s))
}

fn zoned_cell<'a, Z>(
    get: impl Fn(usize) -> Option<NaiveDateTime> + 'a,
    zone: Z,
    mode: Mode,
    format: Vec<Item<'static>>,
) -> Cell<'a>
where
    Z: TimeZone + 'a,
    Z::Offset: Display,
{
    Box::new(move |i, b| {
        if let Some(utc) = get(i) {
            let local = zone.from_utc_datetime(&utc);
            match mode {
                Mode::Csv | Mode::Text => {
                    let _ = write!(b, "{}", local.format_with_items(format.iter()));
                }
                Mode::Json => {
                    let _ = write!(b, "\"{}\"", local.to_rfc3339());
                }
            }
        }
    })
}

fn timestamp_format(unit: TimeUnit, zoned: bool, mode: Mode) -> &'static str {
    let digits = match unit {
        TimeUnit::Second | TimeUnit::Millisecond => 0,
        TimeUnit::Microsecond => 1,
        TimeUnit::Nanosecond => 2,
    };
    match (mode == Mode::Text, zoned) {
        (false, false) => ["%FT%H:%M:%S.%3f", "%FT%H:%M:%S.%6f", "%FT%H:%M:%S.%9f"][digits],
        (false, true) => ["%FT%H:%M:%S.%3f%z", "%FT%H:%M:%S.%6f%z", "%FT%H:%M:%S.%9f%z"][digits],
        (true, false) => ["%F %T%.3f", "%F %T%.6f", "%F %T%.9f"][digits],
        (true, true) => ["%F %T%.3f%:z", "%F %T%.6f%:z", "%F %T%.9f%:z"][digits],
    }
}

fn timestamp_cell<'a>(
    get: impl Fn(usize) -> Option<i64> + 'a,
    unit: TimeUnit,
    tz: Option<&str>,
    mode: Mode,
) -> Result<Cell<'a>, String> {
    let at = move |i: usize| get(i).and_then(|v| naive_datetime(v, &unit));
    match tz {
        None => {
            let format = items(timestamp_format(unit, false, mode));
            Ok(Box::new(move |i, b| {
                if let Some(dt) = at(i) {
                    match mode {
                        Mode::Csv | Mode::Text => {
                            let _ = write!(b, "{}", dt.format_with_items(format.iter()));
                        }
                        Mode::Json => {
                            let _ = write!(b, "\"{}\"", dt);
                        }
                    }
                }
            }))
        }
        Some(name) => {
            let format = items(timestamp_format(unit, true, mode));
            match parse_zone(name)? {
                Zone::Fixed(z) => Ok(zoned_cell(at, z, mode, format)),
                Zone::Named(z) => Ok(zoned_cell(at, z, mode, format)),
            }
        }
    }
}

fn time_cell<'a>(get: impl Fn(usize) -> (i64, u32) + 'a, mode: Mode) -> Cell<'a> {
    let format = items(if mode == Mode::Text { "%T%.f" } else { "%T%.9f" });
    Box::new(move |i, b| {
        let (secs, nanos) = get(i);
        let time = u32::try_from(secs)
            .ok()
            .and_then(|s| NaiveTime::from_num_seconds_from_midnight_opt(s, nanos));
        if let Some(t) = time {
            match mode {
                Mode::Csv | Mode::Text => {
                    let _ = write!(b, "{}", t.format_with_items(format.iter()));
                }
                Mode::Json => {
                    let _ = write!(b, "\"{}\"", t);
                }
            }
        }
    })
}

macro_rules! int_cell {
    ($arr:expr, $t:ty) => {{
        let a = $arr.as_primitive::<$t>();
        Box::new(move |i: usize, b: &mut Vec<u8>| {
            let _ = write!(b, "{}", a.value(i));
        }) as Cell
    }};
}

fn string_cell<'a>(get: impl Fn(usize) -> &'a str + 'a, mode: Mode) -> Cell<'a> {
    match mode {
        Mode::Csv => Box::new(move |i, b| push_csv_str(b, get(i))),
        Mode::Json => Box::new(move |i, b| push_json_str(b, get(i))),
        Mode::Text => Box::new(move |i, b| b.extend_from_slice(get(i).as_bytes())),
    }
}

fn binary_cell<'a>(get: impl Fn(usize) -> &'a [u8] + 'a, mode: Mode) -> Cell<'a> {
    Box::new(move |i, b| {
        let bytes = get(i);
        if bytes.is_empty() && mode == Mode::Csv {
            b.extend_from_slice(b"\"\"");
        } else {
            put_quoted(b, mode, |b| push_hex(b, bytes));
        }
    })
}

fn build_cell<'a>(arr: &'a dyn Array, mode: Mode) -> Result<Cell<'a>, String> {
    let cell: Cell<'a> = match arr.data_type() {
        DataType::Null => Box::new(move |_, b| {
            if mode == Mode::Json {
                b.extend_from_slice(b"null");
            }
        }),
        DataType::Boolean => {
            let a = arr.as_boolean();
            Box::new(move |i, b| b.extend_from_slice(if a.value(i) { &b"true"[..] } else { &b"false"[..] }))
        }
        DataType::Int8 => int_cell!(arr, Int8Type),
        DataType::Int16 => int_cell!(arr, Int16Type),
        DataType::Int32 => int_cell!(arr, Int32Type),
        DataType::Int64 => int_cell!(arr, Int64Type),
        DataType::UInt8 => int_cell!(arr, UInt8Type),
        DataType::UInt16 => int_cell!(arr, UInt16Type),
        DataType::UInt32 => int_cell!(arr, UInt32Type),
        DataType::UInt64 => int_cell!(arr, UInt64Type),
        DataType::Float16 => {
            let a = arr.as_primitive::<Float16Type>();
            Box::new(move |i, b| push_f32(b, a.value(i).to_f32(), mode))
        }
        DataType::Float32 => {
            let a = arr.as_primitive::<Float32Type>();
            Box::new(move |i, b| push_f32(b, a.value(i), mode))
        }
        DataType::Float64 => {
            let a = arr.as_primitive::<Float64Type>();
            Box::new(move |i, b| push_f64(b, a.value(i), mode))
        }
        DataType::Utf8 => {
            let a = arr.as_string::<i32>();
            string_cell(move |i| a.value(i), mode)
        }
        DataType::LargeUtf8 => {
            let a = arr.as_string::<i64>();
            string_cell(move |i| a.value(i), mode)
        }
        DataType::Utf8View => {
            let a = arr.as_string_view();
            string_cell(move |i| a.value(i), mode)
        }
        DataType::Binary => {
            let a = arr.as_binary::<i32>();
            binary_cell(move |i| a.value(i), mode)
        }
        DataType::LargeBinary => {
            let a = arr.as_binary::<i64>();
            binary_cell(move |i| a.value(i), mode)
        }
        DataType::BinaryView => {
            let a = arr.as_binary_view();
            binary_cell(move |i| a.value(i), mode)
        }
        DataType::FixedSizeBinary(_) => {
            let a = arr.as_fixed_size_binary();
            binary_cell(move |i| a.value(i), mode)
        }
        DataType::Date32 => {
            let a = arr.as_primitive::<Date32Type>();
            Box::new(move |i, b| {
                if let Some(d) = a
                    .value(i)
                    .checked_add(UNIX_EPOCH_DAYS_FROM_CE)
                    .and_then(NaiveDate::from_num_days_from_ce_opt)
                {
                    put_quoted(b, mode, |b| {
                        let _ = write!(b, "{}", d);
                    });
                }
            })
        }
        DataType::Time32(TimeUnit::Second) => {
            let a = arr.as_primitive::<Time32SecondType>();
            time_cell(move |i| (a.value(i) as i64, 0), mode)
        }
        DataType::Time32(TimeUnit::Millisecond) => {
            let a = arr.as_primitive::<Time32MillisecondType>();
            time_cell(
                move |i| {
                    let v = a.value(i) as i64;
                    (v.div_euclid(1_000), (v.rem_euclid(1_000) * 1_000_000) as u32)
                },
                mode,
            )
        }
        DataType::Time64(TimeUnit::Microsecond) => {
            let a = arr.as_primitive::<Time64MicrosecondType>();
            time_cell(
                move |i| {
                    let v = a.value(i);
                    (v.div_euclid(1_000_000), (v.rem_euclid(1_000_000) * 1_000) as u32)
                },
                mode,
            )
        }
        DataType::Time64(TimeUnit::Nanosecond) => {
            let a = arr.as_primitive::<Time64NanosecondType>();
            time_cell(
                move |i| {
                    let v = a.value(i);
                    (v.div_euclid(1_000_000_000), v.rem_euclid(1_000_000_000) as u32)
                },
                mode,
            )
        }
        DataType::Timestamp(unit, tz) => {
            let unit = unit.clone();
            let tz = tz.as_deref();
            match unit {
                TimeUnit::Second => {
                    let a = arr.as_primitive::<TimestampSecondType>();
                    timestamp_cell(move |i| Some(a.value(i)), unit, tz, mode)?
                }
                TimeUnit::Millisecond => {
                    let a = arr.as_primitive::<TimestampMillisecondType>();
                    timestamp_cell(move |i| Some(a.value(i)), unit, tz, mode)?
                }
                TimeUnit::Microsecond => {
                    let a = arr.as_primitive::<TimestampMicrosecondType>();
                    timestamp_cell(move |i| Some(a.value(i)), unit, tz, mode)?
                }
                TimeUnit::Nanosecond => {
                    let a = arr.as_primitive::<TimestampNanosecondType>();
                    timestamp_cell(move |i| Some(a.value(i)), unit, tz, mode)?
                }
            }
        }
        DataType::Decimal128(_, scale) if *scale >= 0 => {
            let a = arr.as_primitive::<Decimal128Type>();
            let scale = *scale as usize;
            Box::new(move |i, b| put_quoted(b, mode, |b| push_decimal(b, a.value(i), scale)))
        }
        other => {
            return Err(format!(
                "datatype {} cannot be written to CSV\n\nConsider using JSON or a binary format.",
                other
            ));
        }
    };
    Ok(cell)
}

/// Колонка, подготовленная к записи: либо обычный массив, либо готовые фрагменты JSON.
enum Prepared {
    Plain(ArrayRef),
    RawJson(ArrayRef),
}

fn prepare(arr: &ArrayRef, mode: Mode) -> Result<Prepared, String> {
    match arr.data_type() {
        DataType::Dictionary(_, value) => {
            let plain = cast(arr.as_ref(), value.as_ref()).map_err(|e| e.to_string())?;
            Ok(Prepared::Plain(plain))
        }
        DataType::List(_)
        | DataType::LargeList(_)
        | DataType::FixedSizeList(_, _)
        | DataType::Struct(_)
        | DataType::Map(_, _)
            if mode == Mode::Json =>
        {
            Ok(Prepared::RawJson(to_json_strings(arr)?))
        }
        DataType::Date64
        | DataType::Decimal32(_, _)
        | DataType::Decimal64(_, _)
        | DataType::Decimal256(_, _)
        | DataType::Duration(_)
        | DataType::Interval(_)
            if mode == Mode::Json =>
        {
            let text = cast(arr.as_ref(), &DataType::Utf8).map_err(|e| e.to_string())?;
            Ok(Prepared::Plain(text))
        }
        _ => Ok(Prepared::Plain(arr.clone())),
    }
}

fn write_batch(batch: &RecordBatch, mode: Mode, out: &mut Vec<u8>) -> Result<(), String> {
    let prepared: Vec<Prepared> = batch
        .columns()
        .iter()
        .map(|c| prepare(c, mode))
        .collect::<Result<_, _>>()?;

    let mut columns: Vec<(&dyn Array, Cell)> = Vec::with_capacity(prepared.len());
    for p in &prepared {
        match p {
            Prepared::Plain(arr) => columns.push((arr.as_ref(), build_cell(arr.as_ref(), mode)?)),
            Prepared::RawJson(arr) => {
                let a = arr.as_string::<i32>();
                let cell: Cell = Box::new(move |i, b| b.extend_from_slice(a.value(i).as_bytes()));
                columns.push((arr.as_ref(), cell));
            }
        }
    }

    let keys: Vec<Vec<u8>> = batch
        .schema()
        .fields()
        .iter()
        .map(|f| {
            let mut key = Vec::new();
            push_json_str(&mut key, f.name());
            key.push(b':');
            key
        })
        .collect();

    for row in 0..batch.num_rows() {
        match mode {
            Mode::Csv | Mode::Text => {
                for (c, (arr, cell)) in columns.iter().enumerate() {
                    if c > 0 {
                        out.push(b',');
                    }
                    if !arr.is_null(row) {
                        cell(row, out);
                    }
                }
            }
            Mode::Json => {
                out.push(b'{');
                for (c, (arr, cell)) in columns.iter().enumerate() {
                    if c > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(&keys[c]);
                    if arr.is_null(row) {
                        out.extend_from_slice(b"null");
                    } else {
                        cell(row, out);
                    }
                }
                out.push(b'}');
            }
        }
        out.push(b'\n');
    }
    Ok(())
}

/// Текст значений колонки (NULL остаётся NULL) в виде, который даёт polars при приведении к строке:
/// числа с плавающей точкой кратчайшей записью, дата и время с фиксированным числом знаков, decimal с масштабом.
pub fn text_column(arr: &ArrayRef) -> Result<ArrayRef, String> {
    let prepared = prepare(arr, Mode::Text)?;
    let plain = match &prepared {
        Prepared::Plain(a) => a,
        Prepared::RawJson(_) => unreachable!("raw JSON is only prepared in JSON mode"),
    };
    let cell = build_cell(plain.as_ref(), Mode::Text)?;
    let mut builder = arrow::array::StringBuilder::with_capacity(arr.len(), arr.len() * 16);
    let mut scratch = Vec::new();
    for row in 0..plain.len() {
        if plain.is_null(row) {
            builder.append_null();
        } else {
            scratch.clear();
            cell(row, &mut scratch);
            builder.append_value(String::from_utf8_lossy(&scratch));
        }
    }
    Ok(Arc::new(builder.finish()))
}

pub fn csv_rows(batch: &RecordBatch, out: &mut Vec<u8>) -> Result<(), String> {
    write_batch(batch, Mode::Csv, out)
}

pub fn jsonl_rows(batch: &RecordBatch, out: &mut Vec<u8>) -> Result<(), String> {
    write_batch(batch, Mode::Json, out)
}

pub fn csv_header(schema: &Schema) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, field) in schema.fields().iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        push_csv_str(&mut out, field.name());
    }
    out.push(b'\n');
    out
}

/// Проверяет до записи, что все типы схемы можно записать в выбранный формат.
pub fn check_schema(schema: &Schema, mode: Mode) -> Result<(), String> {
    let batch = RecordBatch::new_empty(Arc::new(schema.clone()));
    let mut scratch = Vec::new();
    write_batch(&batch, mode, &mut scratch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{
        BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float32Array, Float64Array, Int32Array, StringArray,
        Time64NanosecondArray, TimestampMicrosecondArray, TimestampNanosecondArray,
    };
    use arrow::datatypes::Field;
    use std::sync::Arc;

    fn batch(columns: Vec<(&str, ArrayRef)>) -> RecordBatch {
        let fields: Vec<Field> = columns
            .iter()
            .map(|(n, a)| Field::new(*n, a.data_type().clone(), true))
            .collect();
        RecordBatch::try_new(
            Arc::new(Schema::new(fields)),
            columns.into_iter().map(|(_, a)| a).collect(),
        )
        .unwrap()
    }

    fn csv(b: &RecordBatch) -> String {
        let mut out = csv_header(b.schema().as_ref());
        csv_rows(b, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn jsonl(b: &RecordBatch) -> String {
        let mut out = Vec::new();
        jsonl_rows(b, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn empty_string_differs_from_null() {
        let a: ArrayRef = Arc::new(StringArray::from(vec![
            Some(""),
            None,
            Some("x"),
            Some("a,b"),
            Some("q\""),
        ]));
        let n: ArrayRef = Arc::new(Int32Array::from(vec![1, 2, 3, 4, 5]));
        let b = batch(vec![("s", a), ("n", n)]);
        assert_eq!(csv(&b), "s,n\n\"\",1\n,2\nx,3\n\"a,b\",4\n\"q\"\"\",5\n");
        assert_eq!(
            jsonl(&b),
            "{\"s\":\"\",\"n\":1}\n{\"s\":null,\"n\":2}\n{\"s\":\"x\",\"n\":3}\n{\"s\":\"a,b\",\"n\":4}\n{\"s\":\"q\\\"\",\"n\":5}\n"
        );
    }

    #[test]
    fn floats_use_shortest_form() {
        let a: ArrayRef = Arc::new(Float64Array::from(vec![1.0, 0.1, 1e300, f64::NAN, f64::INFINITY, -2.5]));
        let b = batch(vec![("f", a)]);
        let text = csv(&b);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "1.0");
        assert_eq!(lines[2], "0.1");
        assert_eq!(lines[4], "NaN");
        assert_eq!(lines[5], "inf");
        assert_eq!(lines[6], "-2.5");
        let json = jsonl(&b);
        let lines: Vec<&str> = json.lines().collect();
        assert_eq!(lines[0], "{\"f\":1.0}");
        assert_eq!(lines[3], "{\"f\":null}");
        assert_eq!(lines[4], "{\"f\":null}");

        let f: ArrayRef = Arc::new(Float32Array::from(vec![0.1f32, 3.0]));
        let b = batch(vec![("f", f)]);
        assert_eq!(csv(&b), "f\n0.1\n3.0\n");
    }

    #[test]
    fn temporal_values_match_polars() {
        let date: ArrayRef = Arc::new(Date32Array::from(vec![Some(19724), None]));
        let time: ArrayRef = Arc::new(Time64NanosecondArray::from(vec![
            Some(10 * 3600 * 1_000_000_000 + 11 * 60 * 1_000_000_000 + 12 * 1_000_000_000 + 500_000_000),
            Some(0),
        ]));
        let micros: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1_704_164_645_123_456),
            Some(0),
        ]));
        let b = batch(vec![("d", date), ("t", time), ("ts", micros)]);
        assert_eq!(
            csv(&b),
            "d,t,ts\n2024-01-02,10:11:12.500000000,2024-01-02T03:04:05.123456\n,00:00:00.000000000,1970-01-01T00:00:00.000000\n"
        );
        assert_eq!(
            jsonl(&b),
            "{\"d\":\"2024-01-02\",\"t\":\"10:11:12.500\",\"ts\":\"2024-01-02 03:04:05.123456\"}\n\
             {\"d\":null,\"t\":\"00:00:00\",\"ts\":\"1970-01-01 00:00:00\"}\n"
        );
    }

    #[test]
    fn timestamps_with_zone() {
        let a: ArrayRef =
            Arc::new(TimestampNanosecondArray::from(vec![Some(1_704_164_645_123_456_789)]).with_timezone("UTC"));
        let b = batch(vec![("ts", a)]);
        assert_eq!(csv(&b), "ts\n2024-01-02T03:04:05.123456789+0000\n");
        assert_eq!(jsonl(&b), "{\"ts\":\"2024-01-02T03:04:05.123456789+00:00\"}\n");

        let a: ArrayRef =
            Arc::new(TimestampMicrosecondArray::from(vec![Some(1_704_164_645_000_000)]).with_timezone("+03:00"));
        let b = batch(vec![("ts", a)]);
        assert_eq!(csv(&b), "ts\n2024-01-02T06:04:05.000000+0300\n");
        assert_eq!(jsonl(&b), "{\"ts\":\"2024-01-02T06:04:05+03:00\"}\n");

        let a: ArrayRef =
            Arc::new(TimestampMicrosecondArray::from(vec![Some(1_704_164_645_000_000)]).with_timezone("Europe/Moscow"));
        let b = batch(vec![("ts", a)]);
        assert_eq!(csv(&b), "ts\n2024-01-02T06:04:05.000000+0300\n");
    }

    #[test]
    fn decimal_binary_and_bool() {
        let dec: ArrayRef = Arc::new(
            Decimal128Array::from(vec![Some(123456780000i128), Some(-5), None])
                .with_precision_and_scale(20, 10)
                .unwrap(),
        );
        let bin: ArrayRef = Arc::new(BinaryArray::from_opt_vec(vec![
            Some(&[0u8, 255][..]),
            Some(&b""[..]),
            None,
        ]));
        let flag: ArrayRef = Arc::new(BooleanArray::from(vec![Some(true), Some(false), None]));
        let b = batch(vec![("dec", dec), ("bin", bin), ("flag", flag)]);
        assert_eq!(
            csv(&b),
            "dec,bin,flag\n12.3456780000,00ff,true\n-0.0000000005,\"\",false\n,,\n"
        );
        assert_eq!(
            jsonl(&b),
            "{\"dec\":\"12.3456780000\",\"bin\":\"00ff\",\"flag\":true}\n\
             {\"dec\":\"-0.0000000005\",\"bin\":\"\",\"flag\":false}\n\
             {\"dec\":null,\"bin\":null,\"flag\":null}\n"
        );
    }

    #[test]
    fn text_for_hashing_matches_polars_cast() {
        let micros: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![Some(1_704_164_645_123_456), None]));
        let zoned: ArrayRef =
            Arc::new(TimestampMicrosecondArray::from(vec![Some(1_704_164_645_000_000), None]).with_timezone("UTC"));
        let time: ArrayRef = Arc::new(Time64NanosecondArray::from(vec![Some(36_672_500_000_000), Some(0)]));
        let date: ArrayRef = Arc::new(Date32Array::from(vec![Some(19724), None]));
        let flt: ArrayRef = Arc::new(Float64Array::from(vec![Some(1.5), Some(1e300)]));
        let text = |a: &ArrayRef| {
            let t = text_column(a).unwrap();
            t.as_string::<i32>()
                .iter()
                .map(|v| v.map(str::to_string))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            text(&micros),
            vec![Some("2024-01-02 03:04:05.123456".to_string()), None]
        );
        assert_eq!(
            text(&zoned),
            vec![Some("2024-01-02 03:04:05.000000+00:00".to_string()), None]
        );
        assert_eq!(
            text(&time),
            vec![Some("10:11:12.500".to_string()), Some("00:00:00".to_string())]
        );
        assert_eq!(text(&date), vec![Some("2024-01-02".to_string()), None]);
        assert_eq!(text(&flt), vec![Some("1.5".to_string()), Some("1e+300".to_string())]);
    }

    #[test]
    fn header_is_quoted_when_needed() {
        let schema = Schema::new(vec![
            Field::new("a", DataType::Int32, true),
            Field::new("b,c", DataType::Int32, true),
            Field::new("", DataType::Int32, true),
        ]);
        assert_eq!(String::from_utf8(csv_header(&schema)).unwrap(), "a,\"b,c\",\"\"\n");
    }

    #[test]
    fn nested_values_only_in_jsonl() {
        use arrow::array::ListArray;
        let list: ArrayRef = Arc::new(ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
            Some(vec![Some(1), Some(2)]),
            None,
        ]));
        let b = batch(vec![("l", list)]);
        assert_eq!(jsonl(&b), "{\"l\":[1,2]}\n{\"l\":null}\n");
        assert!(check_schema(b.schema().as_ref(), Mode::Csv).is_err());
        assert!(check_schema(b.schema().as_ref(), Mode::Json).is_ok());
    }
}
