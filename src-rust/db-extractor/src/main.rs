//! db-extractor: выгрузка из БД в Parquet/CSV/Arrow IPC/JSONL.

mod cli;
mod extract;
mod guard;
mod run;
mod textfmt;
mod textio;
mod transform;
mod writers;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

use clap::{CommandFactory, Parser};
use cli::{Cli, CONN_ENV_VAR};
use dbx_core::annotations::{extract_dx_params, QueryParams};
use dbx_core::sqlsplit::split_sql_queries;

fn say(quiet: bool, message: &str) {
    if !quiet {
        println!("{}", message);
    }
}

/// Все .sql в папке (в том числе вложенных) в стабильном порядке; расширение без учёта регистра.
fn sql_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase().ends_with(".sql"))
                .unwrap_or(false)
            {
                files.push(path);
            }
        }
    }
    let key = |p: &PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    files.sort_by_key(|p| key(p));
    dirs.sort_by_key(|p| key(p));
    found.extend(files);
    for d in dirs {
        sql_files(&d, found);
    }
}

fn output_key(path: &str) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let text = abs.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn main() {
    guard::install_quiet_panic_hook();
    let args = Cli::parse();
    let quiet = args.quiet;
    let warn = |message: &str| {
        if !quiet {
            eprintln!("Warning: {}", message);
        }
    };

    let env_conn = std::env::var(CONN_ENV_VAR).ok().filter(|v| !v.is_empty());
    if args.conn.as_deref().unwrap_or("").is_empty() && args.conn_file.is_none() && env_conn.is_none() {
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                format!(
                    "Either --conn or --conn-file must be provided (or set {}).",
                    CONN_ENV_VAR
                ),
            )
            .exit();
    }

    let conn: String = if let Some(path) = &args.conn_file {
        if args.conn.is_some() {
            warn("Both --conn and --conn-file are given; --conn-file is used.");
        }
        match textio::read_text(Path::new(path), "utf-8-sig") {
            Ok(text) => text.trim().to_string(),
            Err(e) => {
                say(quiet, &format!("Error reading connection file '{}': {}", path, e));
                exit(1);
            }
        }
    } else if let Some(c) = args.conn.clone().filter(|c| !c.is_empty()) {
        c
    } else {
        env_conn.unwrap_or_default().trim().to_string()
    };
    if conn.is_empty() {
        say(quiet, "Error: the connection string is empty.");
        exit(1);
    }

    let base = args.base_options();
    let mut queries: Vec<QueryParams> = Vec::new();

    if let Some(query_file) = &args.query_file {
        let path = Path::new(query_file);
        let files: Vec<PathBuf> = if path.is_dir() {
            let mut found = Vec::new();
            sql_files(path, &mut found);
            found
        } else if path.is_file() {
            vec![path.to_path_buf()]
        } else {
            say(quiet, &format!("Error: Path '{}' does not exist.", query_file));
            exit(1);
        };

        for file in files {
            let content = match textio::read_text(&file, &args.encoding) {
                Ok(c) => c,
                Err(e) => {
                    say(quiet, &format!("Error reading query file {}: {}", file.display(), e));
                    exit(1);
                }
            };
            let statements = split_sql_queries(&content);
            if statements.is_empty() {
                warn(&format!("No SQL statements found in {}; skipped.", file.display()));
            }
            for statement in statements {
                let item = extract_dx_params(&statement, &base);
                if item.out.as_deref().unwrap_or("").is_empty() {
                    let head: String = statement.chars().take(50).collect();
                    say(
                        quiet,
                        &format!(
                            "Error: Output file not specified for a query in {}:\n{}...\nPlease specify --out argument \
                             or add '-- @DX_OUT: filename.parquet' to the query.",
                            file.display(),
                            head
                        ),
                    );
                    exit(1);
                }
                queries.push(item);
            }
        }
        if queries.is_empty() {
            say(quiet, &format!("Error: no SQL queries found in '{}'.", query_file));
            exit(1);
        }
    } else {
        let statements = split_sql_queries(args.query.as_deref().unwrap_or(""));
        if statements.len() != 1 {
            if statements.is_empty() {
                say(quiet, "Error: --query contains no SQL statement.");
            } else {
                say(
                    quiet,
                    "Error: --query accepts a single statement; use --query-file for several queries.",
                );
            }
            exit(1);
        }
        let item = extract_dx_params(&statements[0], &base);
        if item.out.as_deref().unwrap_or("").is_empty() {
            say(
                quiet,
                "Error: --out argument is required when using --query (or add '-- @DX_OUT: filename.parquet' to the query)",
            );
            exit(1);
        }
        queries.push(item);
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    for (idx, item) in queries.iter().enumerate() {
        let out = item.out.clone().unwrap_or_default();
        let key = output_key(&out);
        if let Some(first) = seen.get(&key) {
            say(
                quiet,
                &format!(
                    "Error: queries {} and {} write to the same file '{}'. Give each query its own \
                     '-- @DX_OUT: filename' or use --query-file with separate outputs.",
                    first + 1,
                    idx + 1,
                    out
                ),
            );
            exit(1);
        }
        seen.insert(key, idx);
    }

    say(
        quiet,
        &format!(
            "Connecting to {}...",
            if conn.contains('@') {
                conn.rsplit('@').next().unwrap_or("database")
            } else {
                "database"
            }
        ),
    );

    let ctx = run::Ctx::new(
        conn,
        quiet,
        args.batch_size.max(1),
        args.hash_salt
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_bytes().to_vec()),
    );
    let ok = run::run_all(&queries, args.workers, args.continue_on_error, &ctx);
    exit(if ok { 0 } else { 1 });
}
