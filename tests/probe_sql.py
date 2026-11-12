#!/usr/bin/env python3
"""Execute every SQL string literal in the host's store layer against the real schema.

`cargo check` proves Rust compiles, not that the SQL does. This pulls each
r#"..."# literal out of src-tauri/src/db and src-tauri/src/engine, splits on ';',
and prepares it against an in-memory DB built from src-tauri/migrations. A
missing column, a renamed table, or a syntax typo is reported as a failure.

Prepare-only: no rows are needed, and no data is written. Run:
    python -I tests/probe_sql.py
"""
import glob
import io
import os
import re
import sqlite3
import sys

KEYWORDS = re.compile(r"(?is)^\s*(select|insert|update|delete|with|pragma|create)\b")


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    con = sqlite3.connect(":memory:")
    con.execute("pragma foreign_keys = on")
    for f in sorted(glob.glob(os.path.join(root, "src-tauri", "migrations", "0*.sql"))):
        con.executescript(io.open(f, encoding="utf-8").read())
    # migrate::apply creates its own bookkeeping table before it queries it;
    # the probe must model that, or it reports a false failure for the four
    # statements that touch it.
    con.execute(
        "create table if not exists _kiri_migrations ("
        "version integer primary key, "
        "applied_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')))"
    )
    con.execute("insert or ignore into _kiri_migrations (version) values (1)")

    # Statements in commands.rs are normal strings too. Handle both.
    files = (
        glob.glob(os.path.join(root, "src-tauri", "src", "db", "*.rs"))
        + glob.glob(os.path.join(root, "src-tauri", "src", "engine", "*.rs"))
    )

    checked = fails = 0
    for path in files:
        src = io.open(path, encoding="utf-8").read()
        spans = []
        for m in re.finditer(r'r#"(.*?)"#', src, re.S):
            spans.append((m.start(), m.group(1), "raw"))
        for m in re.finditer(r'"((?:[^"\\]|\\.)*)"', src):
            body = m.group(1)
            if not body.lstrip().startswith(("select", "insert", "update", "delete", "Select", "Insert")):
                continue
            spans.append((m.start(), body, "normal"))
        for offset, body, kind in spans:
            line = src[:offset].count("\n") + 1
            sql = body.replace("\\n", " ")
            if not KEYWORDS.match(sql):
                continue
            for piece in sql.split(";"):
                piece = piece.strip()
                if not piece or not KEYWORDS.match(piece):
                    continue
                checked += 1
                try:
                    con.execute("explain " + piece)
                except Exception as e:
                    msg = str(e)
                    if any(
                        k in msg
                        for k in (
                            "no such column",
                            "no such table",
                            "has no column",
                            "syntax error",
                            "ambiguous",
                            "no such function",
                        )
                    ):
                        fails += 1
                        print("FAIL %s:%d [%s]" % (os.path.basename(path), line, kind))
                        print("     " + msg)
                        print("     " + re.sub(r"\s+", " ", piece)[:160])

    print("checked:", checked, "failures:", fails)
    return 0 if fails == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
