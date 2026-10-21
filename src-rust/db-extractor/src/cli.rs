//! Командная строка.

use clap::{ArgGroup, Parser};
use dbx_core::annotations::BaseOptions;

pub const CONN_ENV_VAR: &str = "DB_EXTRACTOR_CONN";

#[derive(Parser, Debug)]
#[command(
    name = "db-extractor",
    version,
    about = "Fast DB Extractor using Arrow + ConnectorX (Rust build, streaming)",
    infer_long_args = true,
    group(ArgGroup::new("source").required(true).args(["query", "query_file"]))
)]
pub struct Cli {
    /// Database connection string (e.g., postgresql://user:pass@host:port/db)
    #[arg(long, allow_hyphen_values = true)]
    pub conn: Option<String>,

    /// Path to file containing the database connection string
    #[arg(long)]
    pub conn_file: Option<String>,

    /// SQL query string
    #[arg(long, allow_hyphen_values = true)]
    pub query: Option<String>,

    /// Path to .sql file (or folder with .sql files) containing the queries
    #[arg(long)]
    pub query_file: Option<String>,

    /// Output file path (default if not specified in query)
    #[arg(long, allow_hyphen_values = true)]
    pub out: Option<String>,

    /// Output format (default: infer from output extension)
    #[arg(long, value_parser = ["csv", "parquet", "arrow", "jsonl"])]
    pub format: Option<String>,

    /// Compression algorithm and optional level (e.g., 'zstd', 'zstd:10', 'gzip')
    #[arg(long, allow_hyphen_values = true)]
    pub compression: Option<String>,

    /// Fill nulls with default values ('null' for text, 0 for numbers) in specified columns (or all if not specified)
    #[arg(long, num_args = 0..)]
    pub fill_nulls: Option<Vec<String>>,

    /// Drop rows with nulls in specified columns (or all if no columns specified)
    #[arg(long, num_args = 0..)]
    pub drop_nulls: Option<Vec<String>>,

    /// Convert datetimes to UTC
    #[arg(long)]
    pub utc: bool,

    /// List of columns to convert to categorical
    #[arg(long, num_args = 1..)]
    pub categorize: Option<Vec<String>>,

    /// List of columns to hash (e.g. for PII)
    #[arg(long, num_args = 1..)]
    pub hash_columns: Option<Vec<String>>,

    /// Cast complex/database-specific types (Decimal, Datetime, nested, unknown) to universal Parquet/Arrow types
    #[arg(long)]
    pub cast_universal: bool,

    /// Column name to partition on (for connectorx)
    #[arg(long)]
    pub partition_on: Option<String>,

    /// Number of partitions (requires --partition-on)
    #[arg(long, allow_negative_numbers = true)]
    pub partitions: Option<i64>,

    /// Continue executing remaining queries if one fails
    #[arg(long)]
    pub continue_on_error: bool,

    /// Number of concurrent workers
    #[arg(long, default_value_t = 1, allow_negative_numbers = true)]
    pub workers: i64,

    /// Encoding of query files (default: utf-8-sig, i.e. UTF-8 with optional BOM)
    #[arg(long, default_value = "utf-8-sig")]
    pub encoding: String,

    /// Secret salt for --hash-columns: values are hashed with HMAC-SHA256 instead of plain SHA-256
    #[arg(
        long,
        env = "DB_EXTRACTOR_HASH_SALT",
        hide_env_values = true,
        allow_hyphen_values = true
    )]
    pub hash_salt: Option<String>,

    /// Rows per batch while streaming (memory use is proportional to this value)
    #[arg(long, default_value_t = 100_000)]
    pub batch_size: usize,

    /// Suppress console output
    #[arg(short, long)]
    pub quiet: bool,
}

impl Cli {
    pub fn base_options(&self) -> BaseOptions {
        BaseOptions {
            out: self.out.clone(),
            format: self.format.clone(),
            compression: self.compression.clone(),
            partition_on: self.partition_on.clone(),
            partitions: self.partitions,
            fill_nulls: self.fill_nulls.clone(),
            drop_nulls: self.drop_nulls.clone(),
            utc: self.utc,
            categorize: self.categorize.clone().filter(|c| !c.is_empty()),
            hash_columns: self.hash_columns.clone().filter(|c| !c.is_empty()),
            cast_universal: self.cast_universal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut full = vec!["db-extractor"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn query_or_query_file_is_required_and_exclusive() {
        assert!(parse(&["--conn", "x"]).is_err());
        assert!(parse(&["--conn", "x", "--query", "select 1", "--query-file", "a.sql"]).is_err());
        assert!(parse(&["--conn", "x", "--query", "select 1"]).is_ok());
    }

    #[test]
    fn query_may_start_with_a_comment() {
        let c = parse(&["--query", "-- @DX_OUT: a.csv\nselect 1"]).unwrap();
        assert!(c.query.unwrap().starts_with("--"));
    }

    #[test]
    fn bare_fill_nulls_means_all_columns() {
        let c = parse(&["--query", "q", "--fill-nulls", "--utc"]).unwrap();
        assert_eq!(c.fill_nulls, Some(vec![]));
        assert!(c.utc);
        let c = parse(&["--query", "q", "--fill-nulls", "a", "b", "--drop-nulls"]).unwrap();
        assert_eq!(c.fill_nulls, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(c.drop_nulls, Some(vec![]));
        let c = parse(&["--query", "q"]).unwrap();
        assert_eq!(c.fill_nulls, None);
    }

    #[test]
    fn lists_stop_at_next_flag() {
        let c = parse(&["--query", "q", "--hash-columns", "email", "phone", "--cast-universal"]).unwrap();
        assert_eq!(c.hash_columns, Some(vec!["email".to_string(), "phone".to_string()]));
        assert!(c.cast_universal);
    }

    #[test]
    fn format_choices_and_abbreviations() {
        assert!(parse(&["--query", "q", "--format", "xlsx"]).is_err());
        assert_eq!(
            parse(&["--query", "q", "--format", "csv"]).unwrap().format.as_deref(),
            Some("csv")
        );
        assert!(parse(&["--query", "q", "--cont"]).unwrap().continue_on_error);
    }

    #[test]
    fn numeric_options() {
        let c = parse(&["--query", "q", "--workers", "4", "--partitions", "8", "-q"]).unwrap();
        assert_eq!((c.workers, c.partitions, c.quiet), (4, Some(8), true));
        assert_eq!(c.batch_size, 100_000);
    }
}
