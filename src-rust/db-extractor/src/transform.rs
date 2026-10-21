//! Преобразования данных по пакетам (потоково): хеширование, UTC, заполнение и удаление NULL,
//! универсальное приведение типов, категоризация.
//!
//! План строится один раз по схеме результата, затем применяется к каждому пакету строк.
//! Порядок шагов: hash -> utc -> fill-nulls -> drop-nulls -> cast-universal -> categorize.
//! (В Python-версии категоризация стоит раньше, но это лишь способ хранения значений;
//! результат тот же.)

use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray, BooleanArray, Int64Array, PrimitiveArray, StringArray, StringBuilder};
use arrow::compute::kernels::zip::zip;
use arrow::compute::{and, cast, filter, is_not_null};
use arrow::datatypes::{
    DataType, Field, Schema, SchemaRef, TimeUnit, TimestampMicrosecondType, TimestampMillisecondType,
    TimestampNanosecondType, TimestampSecondType,
};
use arrow::record_batch::RecordBatch;
use arrow::util::display::{ArrayFormatter, FormatOptions};
use dbx_core::columns::resolve_columns;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub struct PlanInput<'a> {
    pub hash_columns: Option<&'a [String]>,
    pub hash_salt: Option<&'a [u8]>,
    pub utc: bool,
    pub categorize: Option<&'a [String]>,
    pub fill_nulls: Option<&'a [String]>,
    pub drop_nulls: Option<&'a [String]>,
    pub cast_universal: bool,
    /// Категоризация имеет смысл только для Parquet (словарное кодирование).
    pub parquet_output: bool,
}

pub struct Plan {
    hash: Vec<usize>,
    salt: Option<Vec<u8>>,
    utc: bool,
    fill: Vec<usize>,
    /// None: не удалять строки; Some: индексы колонок, в которых NULL недопустим.
    drop: Option<Vec<usize>>,
    cast_universal: bool,
    categorize: Vec<usize>,
    names: Vec<String>,
    /// Схема результата после преобразований.
    pub out_schema: SchemaRef,
}

fn indices_of(names: &[String], found: &[String]) -> Vec<usize> {
    found.iter().filter_map(|f| names.iter().position(|n| n == f)).collect()
}

impl Plan {
    /// Строит план по схеме. Возвращает план и предупреждения.
    /// Отсутствующая колонка в `hash_columns` это ошибка: иначе данные ушли бы без маскирования.
    pub fn build(schema: &SchemaRef, input: &PlanInput) -> Result<(Plan, Vec<String>), String> {
        let names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
        let mut warnings: Vec<String> = Vec::new();

        let mut hash = Vec::new();
        if let Some(requested) = input.hash_columns.filter(|c| !c.is_empty()) {
            let (found, missing) = resolve_columns(requested, &names);
            if !missing.is_empty() {
                return Err(format!(
                    "Column(s) to hash not found in the result: {}. Nothing was written, \
                     because unmasked data would be exposed.",
                    missing.join(", ")
                ));
            }
            hash = indices_of(&names, &found);
        }

        let mut select = |requested: &[String], option: &str| -> Vec<usize> {
            let (found, missing) = resolve_columns(requested, &names);
            if !missing.is_empty() {
                warnings.push(format!(
                    "Column(s) not found and skipped for {}: {}",
                    option,
                    missing.join(", ")
                ));
            }
            indices_of(&names, &found)
        };

        let categorize_all = match input.categorize.filter(|c| !c.is_empty()) {
            Some(requested) => select(requested, "--categorize"),
            None => Vec::new(),
        };

        let fill = match input.fill_nulls {
            None => Vec::new(),
            Some([]) => (0..names.len()).collect(),
            Some(requested) => select(requested, "--fill-nulls"),
        };

        let drop = match input.drop_nulls {
            None => None,
            Some([]) => Some((0..names.len()).collect()),
            Some(requested) => {
                let idx = select(requested, "--drop-nulls");
                if idx.is_empty() {
                    None
                } else {
                    Some(idx)
                }
            }
        };

        // --cast-universal возвращает категориальные колонки в строки (как в Python-версии);
        // для не-Parquet форматов категоризация ничего не меняет в содержимом.
        let categorize = if input.cast_universal || !input.parquet_output {
            Vec::new()
        } else {
            categorize_all
        };

        let mut plan = Plan {
            hash,
            salt: input.hash_salt.map(|s| s.to_vec()),
            utc: input.utc,
            fill,
            drop,
            cast_universal: input.cast_universal,
            categorize,
            names,
            out_schema: schema.clone(),
        };

        // Схема результата: применяем план к пустому пакету.
        let empty = RecordBatch::new_empty(schema.clone());
        let cols = plan.apply_columns(&empty)?;
        plan.out_schema = plan.schema_of(&cols);
        Ok((plan, warnings))
    }

    fn schema_of(&self, cols: &[ArrayRef]) -> SchemaRef {
        let fields: Vec<Field> = self
            .names
            .iter()
            .zip(cols.iter())
            .map(|(name, col)| Field::new(name, col.data_type().clone(), true))
            .collect();
        Arc::new(Schema::new(fields))
    }

    fn apply_columns(&self, batch: &RecordBatch) -> Result<Vec<ArrayRef>, String> {
        let mut cols: Vec<ArrayRef> = batch.columns().to_vec();

        for &i in &self.hash {
            cols[i] = hash_array(&cols[i], self.salt.as_deref())?;
        }

        if self.utc {
            for c in cols.iter_mut() {
                *c = to_utc(c);
            }
        }

        for &i in &self.fill {
            cols[i] = fill_nulls(&cols[i])?;
        }

        if let Some(idx) = &self.drop {
            cols = drop_null_rows(cols, idx)?;
        }

        if self.cast_universal {
            for c in cols.iter_mut() {
                *c = universal(c)?;
            }
        }

        for &i in &self.categorize {
            cols[i] = categorize(&cols[i])?;
        }
        Ok(cols)
    }

    /// Применяет план к пакету строк.
    pub fn apply(&self, batch: &RecordBatch) -> Result<RecordBatch, String> {
        let cols = self.apply_columns(batch)?;
        RecordBatch::try_new(self.out_schema.clone(), cols).map_err(|e| e.to_string())
    }
}

// --------------------------------------------------------------------------
// hash
// --------------------------------------------------------------------------

fn hex_digest(data: &[u8], salt: Option<&[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes: Vec<u8> = match salt {
        None => Sha256::digest(data).iter().copied().collect(),
        Some(key) => {
            let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts keys of any length");
            mac.update(data);
            mac.finalize().into_bytes().iter().copied().collect()
        }
    };
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn push_hash(b: &mut StringBuilder, value: Option<&[u8]>, salt: Option<&[u8]>) {
    match value {
        None => b.append_null(),
        Some(bytes) => b.append_value(hex_digest(bytes, salt)),
    }
}

/// SHA-256 (или HMAC-SHA-256 с солью) текстового представления значения; NULL остаётся NULL.
pub fn hash_array(arr: &ArrayRef, salt: Option<&[u8]>) -> Result<ArrayRef, String> {
    let n = arr.len();
    let mut b = StringBuilder::with_capacity(n, n * 64);
    match arr.data_type() {
        DataType::Binary => {
            for v in arr.as_binary::<i32>().iter() {
                push_hash(&mut b, v, salt);
            }
        }
        DataType::LargeBinary => {
            for v in arr.as_binary::<i64>().iter() {
                push_hash(&mut b, v, salt);
            }
        }
        DataType::FixedSizeBinary(_) => {
            for v in arr.as_fixed_size_binary().iter() {
                push_hash(&mut b, v, salt);
            }
        }
        _ => {
            let text = cast(arr.as_ref(), &DataType::Utf8).map_err(|e| e.to_string())?;
            for v in text.as_string::<i32>().iter() {
                push_hash(&mut b, v.map(|s| s.as_bytes()), salt);
            }
        }
    }
    Ok(Arc::new(b.finish()))
}

// --------------------------------------------------------------------------
// utc
// --------------------------------------------------------------------------

/// Временные метки помечаются часовым поясом UTC (значения хранятся как моменты времени UTC).
fn to_utc(arr: &ArrayRef) -> ArrayRef {
    match arr.data_type() {
        DataType::Timestamp(TimeUnit::Second, _) => {
            Arc::new(arr.as_primitive::<TimestampSecondType>().clone().with_timezone("UTC"))
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => Arc::new(
            arr.as_primitive::<TimestampMillisecondType>()
                .clone()
                .with_timezone("UTC"),
        ),
        DataType::Timestamp(TimeUnit::Microsecond, _) => Arc::new(
            arr.as_primitive::<TimestampMicrosecondType>()
                .clone()
                .with_timezone("UTC"),
        ),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => Arc::new(
            arr.as_primitive::<TimestampNanosecondType>()
                .clone()
                .with_timezone("UTC"),
        ),
        _ => arr.clone(),
    }
}

// --------------------------------------------------------------------------
// fill-nulls
// --------------------------------------------------------------------------

fn fill_with(arr: &ArrayRef, replacement: ArrayRef) -> Result<ArrayRef, String> {
    if arr.null_count() == 0 {
        return Ok(arr.clone());
    }
    let mask = is_not_null(arr.as_ref()).map_err(|e| e.to_string())?;
    let scalar = arrow::array::Scalar::new(replacement);
    zip(&mask, arr, &scalar).map_err(|e| e.to_string())
}

/// Строки: NULL -> "null"; числа (включая Decimal): NULL -> 0; остальные типы не меняются.
pub fn fill_nulls(arr: &ArrayRef) -> Result<ArrayRef, String> {
    let dt = arr.data_type().clone();
    let is_text = match &dt {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => true,
        DataType::Dictionary(_, value) => matches!(
            value.as_ref(),
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
        ),
        _ => false,
    };
    if is_text {
        // Dictionary и Utf8View приводим к Utf8; повторная категоризация (если нужна) выполняется позже.
        let text: ArrayRef = match &dt {
            DataType::Utf8 | DataType::LargeUtf8 => arr.clone(),
            _ => cast(arr.as_ref(), &DataType::Utf8).map_err(|e| e.to_string())?,
        };
        let word: ArrayRef = Arc::new(StringArray::from(vec!["null"]));
        let word = cast(word.as_ref(), text.data_type()).map_err(|e| e.to_string())?;
        return fill_with(&text, word);
    }
    if dt.is_numeric() {
        let zero: ArrayRef = Arc::new(Int64Array::from(vec![0i64]));
        let zero = cast(zero.as_ref(), &dt).map_err(|e| e.to_string())?;
        return fill_with(arr, zero);
    }
    Ok(arr.clone())
}

// --------------------------------------------------------------------------
// drop-nulls
// --------------------------------------------------------------------------

fn drop_null_rows(cols: Vec<ArrayRef>, idx: &[usize]) -> Result<Vec<ArrayRef>, String> {
    if idx.iter().all(|&i| cols[i].null_count() == 0) {
        return Ok(cols);
    }
    let rows = cols.first().map(|c| c.len()).unwrap_or(0);
    let mut mask = BooleanArray::from(vec![true; rows]);
    for &i in idx {
        if cols[i].null_count() > 0 {
            let present = is_not_null(cols[i].as_ref()).map_err(|e| e.to_string())?;
            mask = and(&mask, &present).map_err(|e| e.to_string())?;
        }
    }
    cols.iter()
        .map(|c| filter(c.as_ref(), &mask).map_err(|e| e.to_string()))
        .collect()
}

// --------------------------------------------------------------------------
// cast-universal
// --------------------------------------------------------------------------

fn to_display_strings(arr: &ArrayRef) -> Result<ArrayRef, String> {
    let opts = FormatOptions::default();
    let formatter = ArrayFormatter::try_new(arr.as_ref(), &opts).map_err(|e| e.to_string())?;
    let mut b = StringBuilder::with_capacity(arr.len(), arr.len() * 16);
    for i in 0..arr.len() {
        if arr.is_null(i) {
            b.append_null();
        } else {
            b.append_value(formatter.value(i).to_string());
        }
    }
    Ok(Arc::new(b.finish()))
}

/// Вложенные значения (списки, структуры, словари) как JSON-строки.
fn to_json_strings(arr: &ArrayRef) -> Result<ArrayRef, String> {
    let schema = Arc::new(Schema::new(vec![Field::new("c", arr.data_type().clone(), true)]));
    let batch = RecordBatch::try_new(schema, vec![arr.clone()]).map_err(|e| e.to_string())?;
    let mut writer = arrow::json::LineDelimitedWriter::new(Vec::new());
    writer.write(&batch).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    let text = String::from_utf8(writer.into_inner()).map_err(|e| e.to_string())?;

    let mut b = StringBuilder::with_capacity(arr.len(), text.len());
    for line in text.lines() {
        // строка вида {"c":<значение>}; для NULL поле опускается и остаётся {}
        if line == "{}" {
            b.append_null();
        } else if let Some(inner) = line.strip_prefix("{\"c\":").and_then(|l| l.strip_suffix('}')) {
            b.append_value(inner);
        } else {
            return Err(format!("unexpected JSON encoder output: {}", line));
        }
    }
    if b.len() != arr.len() {
        return Err("JSON conversion produced an unexpected number of rows".into());
    }
    Ok(Arc::new(b.finish()))
}

fn timestamp_to_naive_micros(arr: &ArrayRef) -> Result<ArrayRef, String> {
    type Out = PrimitiveArray<TimestampMicrosecondType>;
    let out: Out = match arr.data_type() {
        DataType::Timestamp(TimeUnit::Second, _) => arr
            .as_primitive::<TimestampSecondType>()
            .unary_opt(|v| v.checked_mul(1_000_000)),
        DataType::Timestamp(TimeUnit::Millisecond, _) => arr
            .as_primitive::<TimestampMillisecondType>()
            .unary_opt(|v| v.checked_mul(1_000)),
        DataType::Timestamp(TimeUnit::Microsecond, _) => arr.as_primitive::<TimestampMicrosecondType>().unary(|v| v),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            arr.as_primitive::<TimestampNanosecondType>().unary(|v| v / 1_000)
        }
        other => return Err(format!("not a timestamp: {:?}", other)),
    };
    Ok(Arc::new(out))
}

/// Приводит колонку к типам, понятным любому потребителю Parquet/Arrow:
/// - Decimal -> Float64 (при масштабе 0 и точности до 18 знаков -> Int64);
/// - Timestamp -> микросекунды в UTC без часового пояса;
/// - словари -> строки; вложенные типы (списки, структуры, map) -> JSON-строки;
/// - Duration, Interval и любые другие неизвестные типы -> строки.
/// Числа, строки, бинарные данные, даты, время и Boolean не меняются.
pub fn universal(arr: &ArrayRef) -> Result<ArrayRef, String> {
    use DataType::*;
    match arr.data_type() {
        Boolean | Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 | Float16 | Float32 | Float64
        | Utf8 | LargeUtf8 | Utf8View | Binary | LargeBinary | BinaryView | FixedSizeBinary(_) | Date32 | Date64
        | Time32(_) | Time64(_) | Null => Ok(arr.clone()),

        Decimal32(p, s) | Decimal64(p, s) | Decimal128(p, s) | Decimal256(p, s) => {
            if *s == 0 && *p <= 18 {
                cast(arr.as_ref(), &Int64).map_err(|e| e.to_string())
            } else {
                // Через строку: так значение округляется до ближайшего double ровно один раз.
                let text = cast(arr.as_ref(), &Utf8).map_err(|e| e.to_string())?;
                cast(text.as_ref(), &Float64).map_err(|e| e.to_string())
            }
        }

        Timestamp(_, _) => timestamp_to_naive_micros(arr),

        Dictionary(_, _) => match cast(arr.as_ref(), &Utf8) {
            Ok(text) => Ok(text),
            Err(_) => to_json_strings(arr),
        },

        List(_) | LargeList(_) | FixedSizeList(_, _) | ListView(_) | LargeListView(_) | Struct(_) | Map(_, _) => {
            to_json_strings(arr)
        }

        _ => match cast(arr.as_ref(), &Utf8) {
            Ok(text) => Ok(text),
            Err(_) => to_display_strings(arr),
        },
    }
}

// --------------------------------------------------------------------------
// categorize
// --------------------------------------------------------------------------

fn categorize(arr: &ArrayRef) -> Result<ArrayRef, String> {
    if matches!(arr.data_type(), DataType::Dictionary(_, _)) {
        return Ok(arr.clone());
    }
    let text = match arr.data_type() {
        DataType::Utf8 => arr.clone(),
        _ => cast(arr.as_ref(), &DataType::Utf8).map_err(|e| e.to_string())?,
    };
    let dict = DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8));
    cast(text.as_ref(), &dict).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Decimal128Array, Float64Array, Int32Array, LargeStringArray, ListArray, TimestampSecondArray};
    use arrow::datatypes::Int32Type;

    fn strings(arr: &ArrayRef) -> Vec<Option<String>> {
        let a = cast(arr.as_ref(), &DataType::Utf8).unwrap();
        a.as_string::<i32>().iter().map(|v| v.map(|s| s.to_string())).collect()
    }

    fn batch(cols: Vec<(&str, ArrayRef)>) -> RecordBatch {
        let fields: Vec<Field> = cols
            .iter()
            .map(|(n, a)| Field::new(*n, a.data_type().clone(), true))
            .collect();
        RecordBatch::try_new(
            Arc::new(Schema::new(fields)),
            cols.into_iter().map(|(_, a)| a).collect(),
        )
        .unwrap()
    }

    fn input<'a>() -> PlanInput<'a> {
        PlanInput {
            hash_columns: None,
            hash_salt: None,
            utc: false,
            categorize: None,
            fill_nulls: None,
            drop_nulls: None,
            cast_universal: false,
            parquet_output: true,
        }
    }

    #[test]
    fn sha256_known_vector_and_nulls() {
        let arr: ArrayRef = Arc::new(StringArray::from(vec![Some("abc"), None]));
        let h = hash_array(&arr, None).unwrap();
        assert_eq!(
            strings(&h),
            vec![
                Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()),
                None
            ]
        );
    }

    #[test]
    fn hmac_known_vector() {
        let arr: ArrayRef = Arc::new(StringArray::from(vec!["The quick brown fox jumps over the lazy dog"]));
        let h = hash_array(&arr, Some(b"key")).unwrap();
        assert_eq!(
            strings(&h),
            vec![Some(
                "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8".to_string()
            )]
        );
    }

    #[test]
    fn hash_of_number_uses_its_text_form() {
        let arr: ArrayRef = Arc::new(Int32Array::from(vec![Some(123), None]));
        let text: ArrayRef = Arc::new(StringArray::from(vec![Some("123"), None]));
        assert_eq!(
            strings(&hash_array(&arr, None).unwrap()),
            strings(&hash_array(&text, None).unwrap())
        );
    }

    #[test]
    fn missing_hash_column_is_an_error_and_case_is_ignored() {
        let b = batch(vec![("EMAIL", Arc::new(StringArray::from(vec!["a"])) as ArrayRef)]);
        let cols = vec!["email".to_string()];
        let mut i = input();
        i.hash_columns = Some(&cols);
        let (plan, _) = Plan::build(&b.schema(), &i).unwrap();
        let out = plan.apply(&b).unwrap();
        assert_eq!(out.column(0).len(), 1);
        assert_ne!(strings(out.column(0))[0].as_deref(), Some("a"));

        let missing = vec!["nope".to_string()];
        i.hash_columns = Some(&missing);
        let err = Plan::build(&b.schema(), &i).err().unwrap();
        assert!(err.contains("Nothing was written"), "{}", err);
    }

    #[test]
    fn fill_nulls_for_text_and_numbers_only() {
        let s: ArrayRef = Arc::new(LargeStringArray::from(vec![Some("x"), None]));
        let n: ArrayRef = Arc::new(Float64Array::from(vec![None, Some(1.5)]));
        let flag: ArrayRef = Arc::new(BooleanArray::from(vec![None, Some(true)]));
        assert_eq!(
            strings(&fill_nulls(&s).unwrap()),
            vec![Some("x".into()), Some("null".into())]
        );
        let filled = fill_nulls(&n).unwrap();
        assert_eq!(
            filled.as_primitive::<arrow::datatypes::Float64Type>().values().to_vec(),
            vec![0.0, 1.5]
        );
        assert_eq!(fill_nulls(&flag).unwrap().null_count(), 1);
    }

    #[test]
    fn fill_nulls_decimal() {
        let d: ArrayRef = Arc::new(
            Decimal128Array::from(vec![None, Some(12_345_000_000i128)])
                .with_precision_and_scale(38, 10)
                .unwrap(),
        );
        let filled = fill_nulls(&d).unwrap();
        assert_eq!(filled.null_count(), 0);
        assert_eq!(filled.data_type(), &DataType::Decimal128(38, 10));
    }

    #[test]
    fn drop_nulls_subset_and_all() {
        let a: ArrayRef = Arc::new(Int32Array::from(vec![Some(1), None, Some(3)]));
        let b: ArrayRef = Arc::new(StringArray::from(vec![Some("x"), Some("y"), None]));
        let batch = batch(vec![("a", a), ("b", b)]);

        let subset = vec!["A".to_string()];
        let mut i = input();
        i.drop_nulls = Some(&subset);
        let (plan, _) = Plan::build(&batch.schema(), &i).unwrap();
        assert_eq!(plan.apply(&batch).unwrap().num_rows(), 2);

        let all: Vec<String> = vec![];
        i.drop_nulls = Some(&all);
        let (plan, _) = Plan::build(&batch.schema(), &i).unwrap();
        assert_eq!(plan.apply(&batch).unwrap().num_rows(), 1);
    }

    #[test]
    fn unknown_columns_only_warn_for_non_hash_options() {
        let batch = batch(vec![("a", Arc::new(Int32Array::from(vec![1])) as ArrayRef)]);
        let missing = vec!["zzz".to_string()];
        let mut i = input();
        i.fill_nulls = Some(&missing);
        let (_, warnings) = Plan::build(&batch.schema(), &i).unwrap();
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn decimal_becomes_float64_via_text() {
        let d: ArrayRef = Arc::new(
            Decimal128Array::from(vec![Some(1_234_567_890_123i128), None])
                .with_precision_and_scale(38, 10)
                .unwrap(),
        );
        let f = universal(&d).unwrap();
        assert_eq!(f.data_type(), &DataType::Float64);
        let v = f.as_primitive::<arrow::datatypes::Float64Type>();
        assert!((v.value(0) - 123.4567890123).abs() < 1e-12);
        assert!(v.is_null(1));
    }

    #[test]
    fn decimal_scale_zero_becomes_int64() {
        let d: ArrayRef = Arc::new(
            Decimal128Array::from(vec![Some(42i128)])
                .with_precision_and_scale(10, 0)
                .unwrap(),
        );
        assert_eq!(universal(&d).unwrap().data_type(), &DataType::Int64);
    }

    #[test]
    fn timestamp_with_zone_becomes_naive_microseconds() {
        let t: ArrayRef = Arc::new(TimestampSecondArray::from(vec![Some(1), None]).with_timezone("Europe/Moscow"));
        let u = universal(&t).unwrap();
        assert_eq!(u.data_type(), &DataType::Timestamp(TimeUnit::Microsecond, None));
        let v = u.as_primitive::<TimestampMicrosecondType>();
        assert_eq!(v.value(0), 1_000_000);
        assert!(v.is_null(1));
    }

    #[test]
    fn utc_flag_relabels_timestamps() {
        let t: ArrayRef = Arc::new(TimestampSecondArray::from(vec![Some(5)]));
        let b = batch(vec![("t", t)]);
        let mut i = input();
        i.utc = true;
        let (plan, _) = Plan::build(&b.schema(), &i).unwrap();
        let out = plan.apply(&b).unwrap();
        assert_eq!(
            out.column(0).data_type(),
            &DataType::Timestamp(TimeUnit::Second, Some("UTC".into()))
        );
        assert_eq!(out.column(0).as_primitive::<TimestampSecondType>().value(0), 5);
    }

    #[test]
    fn lists_become_json_text() {
        let list =
            ListArray::from_iter_primitive::<Int32Type, _, _>(vec![Some(vec![Some(1), Some(2)]), None, Some(vec![])]);
        let arr: ArrayRef = Arc::new(list);
        let j = universal(&arr).unwrap();
        assert_eq!(
            strings(&j),
            vec![Some("[1,2]".to_string()), None, Some("[]".to_string())]
        );
    }

    #[test]
    fn categorize_only_for_parquet_and_not_with_cast_universal() {
        let s: ArrayRef = Arc::new(StringArray::from(vec!["a", "b", "a"]));
        let b = batch(vec![("s", s)]);
        let cols = vec!["S".to_string()];

        let mut i = input();
        i.categorize = Some(&cols);
        let (plan, _) = Plan::build(&b.schema(), &i).unwrap();
        assert!(matches!(
            plan.out_schema.field(0).data_type(),
            DataType::Dictionary(_, _)
        ));

        i.parquet_output = false;
        let (plan, _) = Plan::build(&b.schema(), &i).unwrap();
        assert_eq!(plan.out_schema.field(0).data_type(), &DataType::Utf8);

        i.parquet_output = true;
        i.cast_universal = true;
        let (plan, _) = Plan::build(&b.schema(), &i).unwrap();
        assert_eq!(plan.out_schema.field(0).data_type(), &DataType::Utf8);
    }

    #[test]
    fn apply_keeps_schema_stable_across_batches() {
        let a1: ArrayRef = Arc::new(StringArray::from(vec![Some("x"), None]));
        let b1 = batch(vec![("a", a1)]);
        let all: Vec<String> = vec![];
        let mut i = input();
        i.fill_nulls = Some(&all);
        let (plan, _) = Plan::build(&b1.schema(), &i).unwrap();
        let a2: ArrayRef = Arc::new(StringArray::from(vec![Some("y")]));
        let b2 = batch(vec![("a", a2)]);
        assert_eq!(plan.apply(&b1).unwrap().schema(), plan.apply(&b2).unwrap().schema());
    }
}
