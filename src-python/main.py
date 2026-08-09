import argparse
import polars as pl
import sys
import os

def main():
    parser = argparse.ArgumentParser(description="Fast DB Extractor using Polars + ConnectorX")
    parser.add_argument("--conn", required=True, help="Database connection string (e.g., postgresql://user:pass@host:port/db)")
    
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--query", help="SQL query string")
    group.add_argument("--query-file", help="Path to .sql file containing the query")
    
    parser.add_argument("--out", required=True, help="Output file path")
    parser.add_argument("--format", choices=["csv", "parquet"], help="Output format (default: infer from --out extension)")
    parser.add_argument("--partition-on", help="Column name to partition on (for connectorx)")
    parser.add_argument("--partitions", type=int, help="Number of partitions (requires --partition-on)")

    args = parser.parse_args()

    # Считываем SQL запрос
    if args.query_file:
        try:
            with open(args.query_file, "r", encoding="utf-8") as f:
                sql = f.read()
        except Exception as e:
            print(f"Error reading query file: {e}")
            sys.exit(1)
    else:
        sql = args.query

    # Определяем формат вывода
    fmt = args.format
    if not fmt:
        ext = os.path.splitext(args.out)[1].lower()
        if ext == ".csv":
            fmt = "csv"
        elif ext == ".parquet":
            fmt = "parquet"
        else:
            print("Error: Could not infer format from output file extension. Please use --format explicitly.")
            sys.exit(1)

    # Параметры партиционирования
    kwargs = {}
    if args.partition_on and args.partitions:
        kwargs["partition_on"] = args.partition_on
        kwargs["partition_num"] = args.partitions
    elif args.partition_on or args.partitions:
        print("Warning: Both --partition-on and --partitions must be provided for partitioning to work. Ignoring partition settings.")

    print(f"Executing query on {args.conn.split('@')[-1] if '@' in args.conn else 'database'}...")
    
    try:
        # Выполнение запроса и загрузка в DataFrame
        df = pl.read_database(
            query=sql,
            connection=args.conn,
            engine="connectorx",
            **kwargs
        )
        print(f"Extracted {len(df)} rows. Saving to {args.out}...")
        
        # Сохранение результата
        if fmt == "csv":
            df.write_csv(args.out)
        elif fmt == "parquet":
            df.write_parquet(args.out)
            
        print("Done successfully!")
        
    except Exception as e:
        print(f"Database Error: {e}")
        sys.exit(1)

if __name__ == "__main__":
    main()
