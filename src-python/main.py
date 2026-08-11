import argparse
import polars as pl
import sys
import os
import re
import datetime
import time
import hashlib
import threading

log_lock = threading.Lock()

def split_sql_queries(sql_content):
    """
    Safely splits SQL by semicolon, ignoring semicolons inside string literals and comments.
    """
    queries = []
    current = []
    in_single_quote = False
    in_double_quote = False
    in_multiline_comment = False
    in_singleline_comment = False
    
    i = 0
    length = len(sql_content)
    while i < length:
        char = sql_content[i]
        next_char = sql_content[i+1] if i + 1 < length else ''
        
        if in_singleline_comment:
            if char == '\n':
                in_singleline_comment = False
            current.append(char)
        elif in_multiline_comment:
            if char == '*' and next_char == '/':
                in_multiline_comment = False
                current.append(char)
                current.append(next_char)
                i += 1
            else:
                current.append(char)
        elif in_single_quote:
            if char == "'":
                in_single_quote = False
            current.append(char)
        elif in_double_quote:
            if char == '"':
                in_double_quote = False
            current.append(char)
        else:
            if char == '-' and next_char == '-':
                in_singleline_comment = True
                current.append(char)
                current.append(next_char)
                i += 1
            elif char == '/' and next_char == '*':
                in_multiline_comment = True
                current.append(char)
                current.append(next_char)
                i += 1
            elif char == "'":
                in_single_quote = True
                current.append(char)
            elif char == '"':
                in_double_quote = True
                current.append(char)
            elif char == ';':
                q = "".join(current).strip()
                if q:
                    queries.append(q)
                current = []
                i += 1
                continue
            else:
                current.append(char)
        i += 1
        
    q = "".join(current).strip()
    if q:
        queries.append(q)
        
    return queries

def extract_dx_params(query_str, args):
    comp_val = args.compression
    comp_lvl = None
    if comp_val and ":" in comp_val:
        parts = comp_val.split(":", 1)
        comp_val = parts[0].lower()
        if parts[1].isdigit():
            comp_lvl = int(parts[1])

    params = {
        "query": query_str,
        "out": args.out,
        "format": args.format,
        "compression": comp_val,
        "compression_level": comp_lvl,
        "partition_on": args.partition_on,
        "partitions": args.partitions,
        "fill_nulls": args.fill_nulls,
        "drop_nulls": args.drop_nulls,
        "utc": args.utc,
        "categorize": args.categorize,
        "hash_columns": args.hash_columns
    }
    
    out_match = re.search(r'--\s*@DX_OUT:\s*([^\s]+)', query_str, re.IGNORECASE)
    if out_match: params["out"] = out_match.group(1)
        
    fmt_match = re.search(r'--\s*@DX_FORMAT:\s*([^\s]+)', query_str, re.IGNORECASE)
    if fmt_match: params["format"] = fmt_match.group(1).lower()

    comp_match = re.search(r'--\s*@DX_COMPRESSION:\s*([^\s]+)', query_str, re.IGNORECASE)
    if comp_match:
        comp_parts = comp_match.group(1).split(':')
        params["compression"] = comp_parts[0].lower()
        if len(comp_parts) > 1 and comp_parts[1].isdigit():
            params["compression_level"] = int(comp_parts[1])

    part_on_match = re.search(r'--\s*@DX_PARTITION_ON:\s*([^\s]+)', query_str, re.IGNORECASE)
    if part_on_match: params["partition_on"] = part_on_match.group(1)
        
    parts_match = re.search(r'--\s*@DX_PARTITIONS:\s*(\d+)', query_str, re.IGNORECASE)
    if parts_match: params["partitions"] = int(parts_match.group(1))

    fill_match = re.search(r'--\s*@DX_FILL_NULLS(?:[:\s]*(.*))?', query_str, re.IGNORECASE)
    if fill_match:
        cols = fill_match.group(1)
        params["fill_nulls"] = [c.strip() for c in cols.split(',')] if cols and cols.strip() else []

    drop_nulls_match = re.search(r'--\s*@DX_DROP_NULLS(?:[:\s]*(.*))?', query_str, re.IGNORECASE)
    if drop_nulls_match: 
        cols = drop_nulls_match.group(1)
        params["drop_nulls"] = [c.strip() for c in cols.split(',')] if cols and cols.strip() else []

    utc_match = re.search(r'--\s*@DX_UTC', query_str, re.IGNORECASE)
    if utc_match: params["utc"] = True

    cat_match = re.search(r'--\s*@DX_CATEGORIZE:\s*(.+)', query_str, re.IGNORECASE)
    if cat_match: params["categorize"] = [c.strip() for c in cat_match.group(1).split(',')]

    hash_match = re.search(r'--\s*@DX_HASH_COLUMNS:\s*(.+)', query_str, re.IGNORECASE)
    if hash_match: params["hash_columns"] = [c.strip() for c in hash_match.group(1).split(',')]
        
    return params

def log_export(status, query, start_time, end_time, out_file=None, row_count=0, error_msg=None):
    log_file = "export_log.txt"
    duration = (end_time - start_time).total_seconds()
    file_size_mb = 0
    if out_file and os.path.exists(out_file):
        file_size_mb = os.path.getsize(out_file) / (1024 * 1024)
        
    clean_query = " ".join(query.split())[:100] + ("..." if len(query) > 100 else "")
    timestamp = start_time.strftime("%Y-%m-%d %H:%M:%S")
    
    with log_lock:
        with open(log_file, "a", encoding="utf-8") as f:
            if status == "SUCCESS":
                f.write(f"[{timestamp}] SUCCESS | Time: {duration:.2f}s | Rows: {row_count} | Size: {file_size_mb:.2f} MB | File: {out_file} | Query: {clean_query}\n")
            else:
                f.write(f"[{timestamp}] ERROR | Time: {duration:.2f}s | Error: {error_msg} | Query: {clean_query}\n")

def process_query(item, idx, total, args):
    current_sql = item["query"]
    current_out = item["out"]
    
    # Определяем формат вывода
    fmt = item["format"]
    if not fmt:
        ext = os.path.splitext(current_out)[1].lower()
        if ext == ".csv":
            fmt = "csv"
        elif ext == ".parquet":
            fmt = "parquet"
        elif ext in [".arrow", ".ipc"]:
            fmt = "arrow"
        elif ext in [".jsonl", ".ndjson"]:
            fmt = "jsonl"
        else:
            if not args.quiet: print(f"Error: Could not infer format from output file '{current_out}'.")
            return False

    if not args.quiet:
        print(f"\n[{idx+1}/{total}] Executing query -> {current_out} ...")
        
    kwargs = {}
    if item["partition_on"] and item["partitions"]:
        kwargs["partition_on"] = item["partition_on"]
        kwargs["partition_num"] = item["partitions"]
        if not args.quiet:
            print(f"  -> Partitioning enabled: on '{item['partition_on']}' with {item['partitions']} partitions")

    start_time = datetime.datetime.now()
    try:
        df = pl.read_database_uri(
            query=current_sql,
            uri=args.conn,
            engine="connectorx",
            **kwargs
        )
        row_count = len(df)
        
        # Data transformations
        if item["hash_columns"]:
            for col in item["hash_columns"]:
                if col in df.columns:
                    df = df.with_columns(
                        pl.col(col).cast(pl.Utf8).map_elements(
                            lambda x: hashlib.sha256(x.encode('utf-8')).hexdigest() if x is not None else None,
                            return_dtype=pl.Utf8
                        ).alias(col)
                    )
        
        if item["utc"]:
            for col, dtype in df.schema.items():
                if isinstance(dtype, pl.Datetime):
                    if dtype.time_zone is None:
                        df = df.with_columns(pl.col(col).dt.replace_time_zone("UTC"))
                    else:
                        df = df.with_columns(pl.col(col).dt.convert_time_zone("UTC"))
                        
        if item["categorize"]:
            for col in item["categorize"]:
                if col in df.columns:
                    df = df.with_columns(pl.col(col).cast(pl.Categorical))
                    
        if item.get("fill_nulls") is not None:
            target_cols = item["fill_nulls"]
            if len(target_cols) == 0:
                target_cols = df.columns
            else:
                target_cols = [c for c in target_cols if c in df.columns]
                
            for c in target_cols:
                dtype = df.schema[c]
                if dtype == pl.Utf8 or dtype == pl.Categorical:
                    df = df.with_columns(pl.col(c).fill_null("null"))
                elif dtype in pl.NUMERIC_DTYPES:
                    df = df.with_columns(pl.col(c).fill_null(0))
                
        if item["drop_nulls"] is not None:
            if len(item["drop_nulls"]) == 0:
                df = df.drop_nulls()
            else:
                valid_cols = [c for c in item["drop_nulls"] if c in df.columns]
                if valid_cols:
                    df = df.drop_nulls(subset=valid_cols)
        
        row_count = len(df)
        if not args.quiet:
            print(f"Extracted and processed {row_count} rows. Saving to {current_out}...")
        
        if fmt == "csv":
            csv_comp = None
            if item["compression"] in ["gzip", "zstd"]:
                csv_comp = item["compression"]
            elif item["compression"] == "uncompressed":
                csv_comp = None
            elif item["compression"]:
                if not args.quiet:
                    print(f"Warning: CSV compression '{item['compression']}' is not commonly supported by standard CSV writers in Polars. Using uncompressed.")
            df.write_csv(current_out) if not csv_comp else df.write_csv(current_out, compression=csv_comp)
        elif fmt == "parquet":
            pq_comp = item["compression"] if item["compression"] else "zstd"
            if item["compression_level"] is not None:
                df.write_parquet(current_out, compression=pq_comp, compression_level=item["compression_level"])
            else:
                df.write_parquet(current_out, compression=pq_comp)
        elif fmt == "arrow":
            arrow_comp = item["compression"] if item["compression"] in ["uncompressed", "lz4", "zstd"] else "uncompressed"
            df.write_ipc(current_out, compression=arrow_comp)
        elif fmt == "jsonl":
            df.write_ndjson(current_out)
            
        end_time = datetime.datetime.now()
        log_export("SUCCESS", current_sql, start_time, end_time, current_out, row_count)
        if not args.quiet:
            print(f"Done saving {current_out}!")
        
        del df
        return True
        
    except Exception as e:
        end_time = datetime.datetime.now()
        log_export("ERROR", current_sql, start_time, end_time, error_msg=str(e))
        if not args.quiet:
            print(f"Database Error on query {idx+1}: {e}")
        return False

def main():
    parser = argparse.ArgumentParser(description="Fast DB Extractor using Polars + ConnectorX")
    parser.add_argument("--conn", help="Database connection string (e.g., postgresql://user:pass@host:port/db)")
    parser.add_argument("--conn-file", help="Path to file containing the database connection string")
    
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--query", help="SQL query string")
    group.add_argument("--query-file", help="Path to .sql file containing the query")
    
    parser.add_argument("--out", required=False, help="Output file path (default if not specified in query)")
    parser.add_argument("--format", choices=["csv", "parquet", "arrow", "jsonl"], help="Output format (default: infer from output extension)")
    parser.add_argument("--compression", help="Compression algorithm and optional level (e.g., 'zstd', 'zstd:10', 'gzip:6')")
    parser.add_argument("--fill-nulls", nargs='*', help="Fill nulls with default values ('null' for text, 0 for numbers) in specified columns (or all if not specified)")
    parser.add_argument("--drop-nulls", nargs='*', help="Drop rows with nulls in specified columns (or all if no columns specified)")
    parser.add_argument("--utc", action="store_true", help="Convert datetimes to UTC")
    parser.add_argument("--categorize", nargs='+', help="List of columns to convert to categorical")
    parser.add_argument("--hash-columns", nargs='+', help="List of columns to hash (e.g. for PII)")
    parser.add_argument("--partition-on", help="Column name to partition on (for connectorx)")
    parser.add_argument("--partitions", type=int, help="Number of partitions (requires --partition-on)")
    parser.add_argument("--continue-on-error", action="store_true", help="Continue executing remaining queries if one fails")
    parser.add_argument("--workers", type=int, default=1, help="Number of concurrent workers (Thread Pool)")
    parser.add_argument("-q", "--quiet", action="store_true", help="Suppress console output")

    args = parser.parse_args()
    
    if not args.conn and not args.conn_file:
        parser.error("Either --conn or --conn-file must be provided.")
        
    if args.conn_file:
        try:
            with open(args.conn_file, "r", encoding="utf-8") as f:
                args.conn = f.read().strip()
        except Exception as e:
            if not args.quiet: print(f"Error reading connection file '{args.conn_file}': {e}")
            sys.exit(1)

    queries_to_run = []

    # Считываем SQL запрос(ы)
    if args.query_file:
        files_to_process = []
        if os.path.isdir(args.query_file):
            for root, _, files in os.walk(args.query_file):
                for f in files:
                    if f.endswith(".sql"):
                        files_to_process.append(os.path.join(root, f))
        elif os.path.isfile(args.query_file):
            files_to_process.append(args.query_file)
        else:
            if not args.quiet: print(f"Error: Path '{args.query_file}' does not exist.")
            sys.exit(1)
            
        for fp in files_to_process:
            try:
                with open(fp, "r", encoding="utf-8") as f:
                    sql_content = f.read()
            except Exception as e:
                if not args.quiet: print(f"Error reading query file {fp}: {e}")
                sys.exit(1)
                
            raw_queries = split_sql_queries(sql_content)
            
            for clean_q in raw_queries:
                item = extract_dx_params(clean_q, args)
                
                if not item["out"]:
                    if not args.quiet: print(f"Error: Output file not specified for a query in {fp}:\n{clean_q[:50]}...\nPlease specify --out argument or add '-- @DX_OUT: filename.parquet' to the query.")
                    sys.exit(1)
                    
                queries_to_run.append(item)
    else:
        if not args.out:
            if not args.quiet: print("Error: --out argument is required when using --query")
            sys.exit(1)
            
        item = extract_dx_params(args.query, args)
        queries_to_run.append(item)

    if not args.quiet:
        print(f"Connecting to {args.conn.split('@')[-1] if '@' in args.conn else 'database'}...")
    
    total = len(queries_to_run)
    has_errors = False
    
    if args.workers > 1:
        from concurrent.futures import ThreadPoolExecutor, as_completed
        with ThreadPoolExecutor(max_workers=args.workers) as executor:
            futures = []
            for idx, item in enumerate(queries_to_run):
                futures.append(executor.submit(process_query, item, idx, total, args))
                
            for future in as_completed(futures):
                success = future.result()
                if not success:
                    has_errors = True
                    if not args.continue_on_error:
                        sys.exit(1)
    else:
        for idx, item in enumerate(queries_to_run):
            success = process_query(item, idx, total, args)
            if not success:
                has_errors = True
                if not args.continue_on_error:
                    sys.exit(1)

    if has_errors:
        sys.exit(1)

if __name__ == "__main__":
    main()
