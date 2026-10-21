//! Потоковая запись пакетов в файл: Parquet, CSV (gzip/zstd), Arrow IPC, JSONL.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use arrow::datatypes::SchemaRef;
use arrow::ipc::writer::{FileWriter, IpcWriteOptions};
use arrow::ipc::CompressionType;
use arrow::record_batch::RecordBatch;
use dbx_core::compression::ResolvedCompression;
use parquet::arrow::ArrowWriter;
use parquet::basic::{BrotliLevel, Compression, GzipLevel, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::textfmt::{self, Mode};

/// Строк в одной группе Parquet.
const ROW_GROUP_ROWS: usize = 262_144;
const DEFAULT_GZIP_LEVEL: u32 = 6;
const DEFAULT_ZSTD_LEVEL: i32 = 3;

pub trait Sink {
    fn write(&mut self, batch: &RecordBatch) -> Result<(), String>;
    fn finish(self: Box<Self>) -> Result<(), String>;
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

enum Out {
    Plain(BufWriter<File>),
    Gzip(flate2::write::GzEncoder<BufWriter<File>>),
    Zstd(zstd::stream::write::Encoder<'static, BufWriter<File>>),
}

impl Out {
    fn open(path: &Path, algo: Option<&str>, level: Option<u32>) -> Result<Out, String> {
        let file = BufWriter::with_capacity(1 << 20, File::create(path).map_err(err)?);
        match algo {
            None => Ok(Out::Plain(file)),
            Some("gzip") => Ok(Out::Gzip(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::new(level.unwrap_or(DEFAULT_GZIP_LEVEL)),
            ))),
            Some("zstd") => {
                let lvl = level.map(|l| l as i32).unwrap_or(DEFAULT_ZSTD_LEVEL);
                Ok(Out::Zstd(zstd::stream::write::Encoder::new(file, lvl).map_err(err)?))
            }
            Some(other) => Err(format!("unsupported compression '{}'", other)),
        }
    }

    fn finish(self) -> io::Result<()> {
        match self {
            Out::Plain(mut w) => w.flush(),
            Out::Gzip(e) => e.finish()?.flush(),
            Out::Zstd(e) => e.finish()?.flush(),
        }
    }
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Out::Plain(w) => w.write(buf),
            Out::Gzip(w) => w.write(buf),
            Out::Zstd(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Out::Plain(w) => w.flush(),
            Out::Gzip(w) => w.flush(),
            Out::Zstd(w) => w.flush(),
        }
    }
}

struct TextSink {
    out: Out,
    buf: Vec<u8>,
    mode: Mode,
}

impl Sink for TextSink {
    fn write(&mut self, batch: &RecordBatch) -> Result<(), String> {
        self.buf.clear();
        match self.mode {
            Mode::Csv => textfmt::csv_rows(batch, &mut self.buf)?,
            Mode::Json | Mode::Text => textfmt::jsonl_rows(batch, &mut self.buf)?,
        }
        self.out.write_all(&self.buf).map_err(err)
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        self.out.finish().map_err(err)
    }
}

struct ParquetSink {
    writer: ArrowWriter<File>,
}

fn parquet_codec(algo: &str, level: Option<u32>) -> Result<Compression, String> {
    Ok(match algo {
        "uncompressed" => Compression::UNCOMPRESSED,
        "snappy" => Compression::SNAPPY,
        "lz4" => Compression::LZ4_RAW,
        "gzip" => Compression::GZIP(GzipLevel::try_new(level.unwrap_or(DEFAULT_GZIP_LEVEL)).map_err(err)?),
        "brotli" => Compression::BROTLI(match level {
            Some(l) => BrotliLevel::try_new(l).map_err(err)?,
            None => BrotliLevel::default(),
        }),
        "zstd" => {
            Compression::ZSTD(ZstdLevel::try_new(level.map(|l| l as i32).unwrap_or(DEFAULT_ZSTD_LEVEL)).map_err(err)?)
        }
        other => return Err(format!("unsupported compression '{}' for parquet", other)),
    })
}

impl Sink for ParquetSink {
    fn write(&mut self, batch: &RecordBatch) -> Result<(), String> {
        self.writer.write(batch).map_err(err)
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        self.writer.close().map(|_| ()).map_err(err)
    }
}

struct IpcSink {
    writer: FileWriter<BufWriter<File>>,
}

impl Sink for IpcSink {
    fn write(&mut self, batch: &RecordBatch) -> Result<(), String> {
        self.writer.write(batch).map_err(err)
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        self.writer.finish().map_err(err)?;
        let mut inner = self.writer.into_inner().map_err(err)?;
        inner.flush().map_err(err)
    }
}

/// Открывает приёмник данных для файла `path`.
pub fn open_sink(
    format: &str,
    path: &Path,
    schema: &SchemaRef,
    compression: &ResolvedCompression,
) -> Result<Box<dyn Sink>, String> {
    let algo = compression.algo.as_deref();
    match format {
        "parquet" => {
            let codec = parquet_codec(algo.unwrap_or("zstd"), compression.level)?;
            let props = WriterProperties::builder()
                .set_compression(codec)
                .set_max_row_group_size(ROW_GROUP_ROWS)
                .build();
            let file = File::create(path).map_err(err)?;
            let writer = ArrowWriter::try_new(file, schema.clone(), Some(props)).map_err(err)?;
            Ok(Box::new(ParquetSink { writer }))
        }
        "csv" => {
            textfmt::check_schema(schema, Mode::Csv)?;
            let mut out = Out::open(path, algo, compression.level)?;
            out.write_all(&textfmt::csv_header(schema)).map_err(err)?;
            Ok(Box::new(TextSink {
                out,
                buf: Vec::new(),
                mode: Mode::Csv,
            }))
        }
        "arrow" => {
            let mut options = IpcWriteOptions::default();
            options = match algo {
                Some("lz4") => options
                    .try_with_compression(Some(CompressionType::LZ4_FRAME))
                    .map_err(err)?,
                Some("zstd") => options.try_with_compression(Some(CompressionType::ZSTD)).map_err(err)?,
                _ => options,
            };
            let file = BufWriter::with_capacity(1 << 20, File::create(path).map_err(err)?);
            let writer = FileWriter::try_new_with_options(file, schema.as_ref(), options).map_err(err)?;
            Ok(Box::new(IpcSink { writer }))
        }
        "jsonl" => {
            textfmt::check_schema(schema, Mode::Json)?;
            let out = Out::open(path, None, None)?;
            Ok(Box::new(TextSink {
                out,
                buf: Vec::new(),
                mode: Mode::Json,
            }))
        }
        other => Err(format!("Unsupported format '{}'.", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Int32Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn sample() -> (SchemaRef, RecordBatch) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, true),
            Field::new("name", DataType::Utf8, true),
        ]));
        let ids: ArrayRef = Arc::new(Int32Array::from(vec![Some(1), Some(2), None]));
        let names: ArrayRef = Arc::new(StringArray::from(vec![Some("a"), None, Some("c,d")]));
        let batch = RecordBatch::try_new(schema.clone(), vec![ids, names]).unwrap();
        (schema, batch)
    }

    fn comp(algo: Option<&str>, level: Option<u32>) -> ResolvedCompression {
        ResolvedCompression {
            algo: algo.map(|s| s.to_string()),
            level,
            warnings: vec![],
        }
    }

    #[test]
    fn csv_has_header_for_empty_result_and_quotes_commas() {
        let dir = tempfile::tempdir().unwrap();
        let (schema, batch) = sample();

        let empty = dir.path().join("empty.csv");
        let sink = open_sink("csv", &empty, &schema, &comp(None, None)).unwrap();
        sink.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&empty).unwrap(), "id,name\n");

        let full = dir.path().join("full.csv");
        let mut sink = open_sink("csv", &full, &schema, &comp(None, None)).unwrap();
        sink.write(&batch).unwrap();
        sink.finish().unwrap();
        assert_eq!(std::fs::read_to_string(&full).unwrap(), "id,name\n1,a\n2,\n,\"c,d\"\n");
    }

    #[test]
    fn csv_gzip_and_zstd_roundtrip() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let (schema, batch) = sample();

        let gz = dir.path().join("a.csv"); // имя не обязано заканчиваться на .gz
        let mut sink = open_sink("csv", &gz, &schema, &comp(Some("gzip"), Some(6))).unwrap();
        sink.write(&batch).unwrap();
        sink.finish().unwrap();
        let mut text = String::new();
        flate2::read::GzDecoder::new(File::open(&gz).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.starts_with("id,name\n1,a\n"));

        let zs = dir.path().join("b.csv.zst");
        let mut sink = open_sink("csv", &zs, &schema, &comp(Some("zstd"), None)).unwrap();
        sink.write(&batch).unwrap();
        sink.finish().unwrap();
        let bytes = zstd::stream::decode_all(File::open(&zs).unwrap()).unwrap();
        assert!(String::from_utf8(bytes).unwrap().starts_with("id,name\n1,a\n"));
    }

    #[test]
    fn jsonl_writes_explicit_nulls() {
        let dir = tempfile::tempdir().unwrap();
        let (schema, batch) = sample();
        let path = dir.path().join("a.jsonl");
        let mut sink = open_sink("jsonl", &path, &schema, &comp(None, None)).unwrap();
        sink.write(&batch).unwrap();
        sink.finish().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], r#"{"id":2,"name":null}"#);
    }

    #[test]
    fn parquet_roundtrip_with_each_codec() {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let dir = tempfile::tempdir().unwrap();
        let (schema, batch) = sample();
        for algo in ["uncompressed", "snappy", "gzip", "brotli", "lz4", "zstd"] {
            let path = dir.path().join(format!("{}.parquet", algo));
            let mut sink = open_sink("parquet", &path, &schema, &comp(Some(algo), None)).unwrap();
            sink.write(&batch).unwrap();
            sink.finish().unwrap();
            let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
                .unwrap()
                .build()
                .unwrap();
            let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
            assert_eq!(rows, 3, "{}", algo);
        }
    }

    #[test]
    fn arrow_ipc_roundtrip_with_compression() {
        use arrow::ipc::reader::FileReader;
        let dir = tempfile::tempdir().unwrap();
        let (schema, batch) = sample();
        for algo in [None, Some("lz4"), Some("zstd")] {
            let path = dir.path().join(format!("{}.arrow", algo.unwrap_or("plain")));
            let mut sink = open_sink("arrow", &path, &schema, &comp(algo, None)).unwrap();
            sink.write(&batch).unwrap();
            sink.finish().unwrap();
            let reader = FileReader::try_new(File::open(&path).unwrap(), None).unwrap();
            let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
            assert_eq!(rows, 3);
        }
    }

    #[test]
    fn parquet_empty_result_keeps_schema() {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let dir = tempfile::tempdir().unwrap();
        let (schema, _) = sample();
        let path = dir.path().join("empty.parquet");
        let sink = open_sink("parquet", &path, &schema, &comp(None, None)).unwrap();
        sink.finish().unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap()).unwrap();
        assert_eq!(builder.schema().fields().len(), 2);
    }
}
