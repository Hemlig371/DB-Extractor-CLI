"""Выполняет SQL-файл в ClickHouse по HTTP: один запрос на каждое выражение, разделитель `;` в конце строки."""
import sys
import urllib.error
import urllib.request


def main():
    base, path = sys.argv[1], sys.argv[2]
    with open(path, encoding="utf-8") as f:
        text = f.read()
    statements = [part.strip() for part in text.replace("\r\n", "\n").split(";\n") if part.strip()]
    for statement in statements:
        request = urllib.request.Request(base, data=statement.encode("utf-8"), method="POST")
        try:
            urllib.request.urlopen(request, timeout=120).read()
        except urllib.error.HTTPError as e:
            sys.exit("ClickHouse error: %s\n--- statement ---\n%s" % (e.read().decode("utf-8", "replace"), statement))
    print("%d statements executed" % len(statements))


if __name__ == "__main__":
    main()
