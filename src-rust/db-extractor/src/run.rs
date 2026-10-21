//! Выполнение запросов: потоковое чтение -> преобразования -> атомарная запись -> журнал.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use dbx_core::annotations::QueryParams;
use dbx_core::compression::{resolve_compression, resolve_format};
use dbx_core::logfmt::{format_error, format_success};
use dbx_core::secrets::mask_secrets;

use crate::extract;
use crate::guard::guard;
use crate::transform::{Plan, PlanInput};
use crate::writers::open_sink;

pub struct Ctx {
    pub conn: String,
    pub quiet: bool,
    pub batch_size: usize,
    pub hash_salt: Option<Vec<u8>>,
    pub log_path: PathBuf,
    log_lock: Mutex<()>,
}

impl Ctx {
    pub fn new(conn: String, quiet: bool, batch_size: usize, hash_salt: Option<Vec<u8>>) -> Ctx {
        Ctx {
            conn,
            quiet,
            batch_size,
            hash_salt,
            log_path: PathBuf::from("export_log.txt"),
            log_lock: Mutex::new(()),
        }
    }

    pub fn warn(&self, message: &str) {
        if !self.quiet {
            eprintln!("Warning: {}", message);
        }
    }

    fn say(&self, message: &str) {
        if !self.quiet {
            println!("{}", message);
        }
    }

    fn append_log(&self, line: &str) {
        let _guard = self.log_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

fn timestamp_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Временный файл рядом с целевым: запись атомарна (rename после успеха).
fn temp_path_for(out: &Path) -> PathBuf {
    let dir = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    let tid: String = format!("{:?}", std::thread::current().id())
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    dir.join(format!(".{}.{}.{}.tmp", name, std::process::id(), tid))
}

fn run_query(item: &QueryParams, idx: usize, total: usize, ctx: &Ctx) -> Result<(), String> {
    for message in &item.warnings {
        ctx.warn(message);
    }

    let out = item.out.clone().unwrap_or_default();

    // Всё, что можно проверить без БД, проверяем до запроса.
    let format = resolve_format(item.format.as_deref(), &out)?;
    let compression = resolve_compression(item.compression.as_deref(), item.compression_level, &out, &format)?;
    for message in &compression.warnings {
        ctx.warn(message);
    }

    let partition_on = item.partition_on.as_deref().filter(|c| !c.is_empty());
    let partitions = item.partitions.filter(|n| *n != 0);
    let partition_by: Option<(&str, usize)> = match (partition_on, partitions) {
        (Some(column), Some(n)) => {
            if n < 0 {
                return Err("--partitions must be a positive integer.".into());
            }
            Some((column, n as usize))
        }
        (None, None) => None,
        _ => {
            ctx.warn("Partitioning is disabled: --partition-on and --partitions must be used together.");
            None
        }
    };

    let out_path = Path::new(&out);
    if let Some(dir) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| format!("Cannot create output directory '{}': {}", dir.display(), e))?;
    }

    ctx.say(&format!("\n[{}/{}] Executing query -> {} ...", idx + 1, total, out));
    if let Some((column, n)) = partition_by {
        ctx.say(&format!(
            "  -> Partitioning enabled: on '{}' with {} partitions",
            column, n
        ));
    }

    let started = Instant::now();

    let mut stream = extract::open(&ctx.conn, &item.query, partition_by, ctx.batch_size)?;

    let (plan, plan_warnings) = Plan::build(
        &stream.schema,
        &PlanInput {
            hash_columns: item.hash_columns.as_deref(),
            hash_salt: ctx.hash_salt.as_deref(),
            utc: item.utc,
            categorize: item.categorize.as_deref(),
            fill_nulls: item.fill_nulls.as_deref(),
            drop_nulls: item.drop_nulls.as_deref(),
            cast_universal: item.cast_universal,
            parquet_output: format == "parquet",
        },
    )?;
    for message in &plan_warnings {
        ctx.warn(message);
    }

    // Устройства и каналы (например /dev/stdout) пишем напрямую, обычные файлы через временный.
    let direct = out_path.exists() && !out_path.is_file();
    let target: PathBuf = if direct {
        out_path.to_path_buf()
    } else {
        temp_path_for(out_path)
    };

    let result = stream_to_file(&mut stream, &plan, &format, &target, &compression, &out, ctx);
    let rows = match result {
        Ok(rows) => rows,
        Err(e) => {
            if !direct {
                let _ = fs::remove_file(&target);
            }
            return Err(e);
        }
    };
    if !direct {
        if let Err(e) = fs::rename(&target, out_path) {
            let _ = fs::remove_file(&target);
            return Err(format!("Cannot move the result to '{}': {}", out, e));
        }
    }

    let size_mb = fs::metadata(out_path)
        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0);
    ctx.append_log(&format_success(
        &timestamp_now(),
        started.elapsed().as_secs_f64(),
        rows,
        size_mb,
        &out,
        &item.query,
    ));
    ctx.say(&format!("Done saving {}!", out));
    Ok(())
}

/// Читает пакеты, применяет план и пишет в файл. Возвращает число записанных строк.
fn stream_to_file(
    stream: &mut extract::Stream,
    plan: &Plan,
    format: &str,
    target: &Path,
    compression: &dbx_core::compression::ResolvedCompression,
    out_name: &str,
    ctx: &Ctx,
) -> Result<u64, String> {
    let mut sink = open_sink(format, target, &plan.out_schema, compression)?;
    stream.start()?;

    let mut rows: u64 = 0;
    while let Some(batch) = stream.next_batch()? {
        if batch.num_rows() == 0 {
            continue;
        }
        let batch = plan.apply(&batch)?;
        if batch.num_rows() == 0 {
            continue;
        }
        rows += batch.num_rows() as u64;
        sink.write(&batch)?;
    }

    ctx.say(&format!(
        "Extracted and processed {} rows. Saving to {}...",
        rows, out_name
    ));
    sink.finish()?;
    Ok(rows)
}

/// Выполняет один запрос; ошибка (в том числе паника connectorx) попадает в журнал и на консоль.
pub fn process_query(item: &QueryParams, idx: usize, total: usize, ctx: &Ctx) -> bool {
    let started = Instant::now();
    let result = guard(|| run_query(item, idx, total, ctx)).and_then(|r| r);
    match result {
        Ok(()) => true,
        Err(e) => {
            let raw = if e.trim().is_empty() {
                "unknown error".to_string()
            } else {
                e
            };
            let message = collapse_ws(&mask_secrets(&raw, &ctx.conn));
            ctx.append_log(&format_error(
                &timestamp_now(),
                started.elapsed().as_secs_f64(),
                &message,
                &item.query,
            ));
            ctx.say(&format!("Error on query {}: {}", idx + 1, message));
            false
        }
    }
}

/// Запускает все запросы. Возвращает true, если не было ошибок.
pub fn run_all(items: &[QueryParams], workers: i64, continue_on_error: bool, ctx: &Ctx) -> bool {
    let total = items.len();
    if workers <= 1 {
        let mut ok = true;
        for (idx, item) in items.iter().enumerate() {
            if !process_query(item, idx, total, ctx) {
                ok = false;
                if !continue_on_error {
                    return false;
                }
            }
        }
        return ok;
    }

    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..(workers as usize).min(total.max(1)) {
            scope.spawn(|| loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let idx = next.fetch_add(1, Ordering::SeqCst);
                if idx >= total {
                    break;
                }
                if !process_query(&items[idx], idx, total, ctx) {
                    failed.store(true, Ordering::SeqCst);
                    if !continue_on_error {
                        // Не начатые запросы не запускаем; уже идущие дожидаются завершения.
                        stop.store(true, Ordering::SeqCst);
                    }
                }
            });
        }
    });
    !failed.load(Ordering::SeqCst)
}
