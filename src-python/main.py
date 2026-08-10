import argparse
import polars as pl
import sys
import os
import re
import datetime

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

def extract_dx_params(query_str, default_out):
    out_file = default_out
    partition_on = None
    partitions = None
    
    out_match = re.search(r'--\s*@DX_OUT:\s*([^\s]+)', query_str, re.IGNORECASE)
    if out_match:
        out_file = out_match.group(1)
        
    part_on_match = re.search(r'--\s*@DX_PARTITION_ON:\s*([^\s]+)', query_str, re.IGNORECASE)
    if part_on_match:
        partition_on = part_on_match.group(1)
        
    parts_match = re.search(r'--\s*@DX_PARTITIONS:\s*(\d+)', query_str, re.IGNORECASE)
    if parts_match:
        partitions = int(parts_match.group(1))
        
    return out_file, partition_on, partitions

def log_export(status, query, start_time, end_time, out_file=None, row_count=0, error_msg=None):
    log_file = "export_log.txt"
    duration = (end_time - start_time).total_seconds()
    file_size_mb = 0
    if out_file and os.path.exists(out_file):
        file_size_mb = os.path.getsize(out_file) / (1024 * 1024)
        
    clean_query = " ".join(query.split())[:100] + ("..." if len(query) > 100 else "")
    timestamp = start_time.strftime("%Y-%m-%d %H:%M:%S")
    
    with open(log_file, "a", encoding="utf-8") as f:
        if status == "SUCCESS":
            f.write(f"[{timestamp}] SUCCESS | Time: {duration:.2f}s | Rows: {row_count} | Size: {file_size_mb:.2f} MB | File: {out_file} | Query: {clean_query}\n")
        else:
            f.write(f"[{timestamp}] ERROR | Time: {duration:.2f}s | Error: {error_msg} | Query: {clean_query}\n")

def main():
    parser = argparse.ArgumentParser(description="Fast DB Extractor using Polars + ConnectorX")
    parser.add_argument("--conn", required=True, help="Database connection string (e.g., postgresql://user:pass@host:port/db)")
    
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--query", help="SQL query string")
    group.add_argument("--query-file", help="Path to .sql file containing the query")
    
    parser.add_argument("--out", required=False, help="Output file path (default if not specified in query)")
    parser.add_argument("--format", choices=["csv", "parquet"], help="Output format (default: infer from output extension)")
    parser.add_argument("--partition-on", help="Column name to partition on (for connectorx)")
    parser.add_argument("--partitions", type=int, help="Number of partitions (requires --partition-on)")

    args = parser.parse_args()

    queries_to_run = []

    # Считываем SQL запрос(ы)
    if args.query_file:
        try:
            with open(args.query_file, "r", encoding="utf-8") as f:
                sql_content = f.read()
        except Exception as e:
            print(f"Error reading query file: {e}")
            sys.exit(1)
            
        raw_queries = split_sql_queries(sql_content)
        
        for clean_q in raw_queries:
            out_file, partition_on, partitions = extract_dx_params(clean_q, args.out)
            
            if not out_file:
                print(f"Error: Output file not specified for query:\n{clean_q[:50]}...\nPlease specify --out argument or add '-- @DX_OUT: filename.parquet' to the query.")
                sys.exit(1)
                
            queries_to_run.append({
                "query": clean_q, 
                "out": out_file,
                "partition_on": partition_on or args.partition_on,
                "partitions": partitions or args.partitions
            })
    else:
        if not args.out:
            print("Error: --out argument is required when using --query")
            sys.exit(1)
            
        out_file, partition_on, partitions = extract_dx_params(args.query, args.out)
        queries_to_run = [{
            "query": args.query, 
            "out": out_file,
            "partition_on": partition_on or args.partition_on,
            "partitions": partitions or args.partitions
        }]

    print(f"Connecting to {args.conn.split('@')[-1] if '@' in args.conn else 'database'}...")
    
    for idx, item in enumerate(queries_to_run):
        current_sql = item["query"]
        current_out = item["out"]
        
        # Определяем формат вывода
        fmt = args.format
        if not fmt:
            ext = os.path.splitext(current_out)[1].lower()
            if ext == ".csv":
                fmt = "csv"
            elif ext == ".parquet":
                fmt = "parquet"
            else:
                print(f"Error: Could not infer format from output file '{current_out}'. Please use .csv or .parquet")
                sys.exit(1)

        print(f"\n[{idx+1}/{len(queries_to_run)}] Executing query -> {current_out} ...")
        
        kwargs = {}
        if item["partition_on"] and item["partitions"]:
            kwargs["partition_on"] = item["partition_on"]
            kwargs["partition_num"] = item["partitions"]
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
            print(f"Extracted {row_count} rows. Saving to {current_out}...")
            
            if fmt == "csv":
                df.write_csv(current_out)
            elif fmt == "parquet":
                df.write_parquet(current_out)
                
            end_time = datetime.datetime.now()
            log_export("SUCCESS", current_sql, start_time, end_time, current_out, row_count)
            print(f"Done saving {current_out}!")
            
            # Очищаем память, чтобы следующие запросы могли свободно использовать ОЗУ
            del df
            
        except Exception as e:
            end_time = datetime.datetime.now()
            log_export("ERROR", current_sql, start_time, end_time, error_msg=str(e))
            print(f"Database Error on query {idx+1}: {e}")
            sys.exit(1)

if __name__ == "__main__":
    main()
