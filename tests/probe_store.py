#!/usr/bin/env python3
"""Run the host's real SQL against real SQLite.

`cargo check` proves the Rust types compile. It does not prove the SQL matches
the schema, and a mixed-up column index or a missing column is exactly the class
of bug that ships a blank screen (it did, once: list_sessions read the turn count
as text, every row failed and was silently dropped).

This applies the real migration files unrelated to the app, runs the exact
statements the store layer uses, and asserts the column shapes its row mappers
expectce. Run:  python -I tests/probe_store.py
"""
import os
import sqlite3
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MIG = os.path.join(ROOT, "src-tauri", "migrations")

results = []


def db():
    c = sqlite3.connect(":memory:")
    c.execute("pragma foreign_keys = on")
    for name in sorted(os.listdir(MIG)):
        if name.endswith(".sql"):
            c.executescript(open(os.path.join(MIG, name), encoding="utf-8").read())
    c.execute("insert into projects(id,path,name) values('p','C:/kiri','kiri')")
    c.execute(
        "insert into sessions(id,project_id,model,effort,permission_mode,"
        "status,transcript_path) values('s','p','m','high','default','active','C:/kiri/s.jsonl')"
    )
    return c


def check(name, fn):
    c = db()
    try:
        fn(c)
        results.append((name, True, ""))
    except Exception as e:
        results.append((name, False, "%s: %s" % (type(e).__name__, e)))


def list_sessions(c):
    """The exact SELECT shape from db/history.rs::list_sessions."""
    c.execute(
        "insert into turns(session_id,ordinal,prompt_id,status,prompt_text)"
        " values('s',1,'p1','done','write a note')"
    )
    row = c.execute(
        """select s.id, s.title, s.model, s.effort, s.permission_mode, s.status,
                  s.transcript_path, s.pid, s.fork_of, s.cost_micros,
                  s.created_at, s.updated_at,
                  (select count(*) from turns t where t.session_id = s.id),
                  (select coalesce(min(t.prompt_text), '')
                     from turns t where t.session_id = s.id and t.ordinal = 1)
             from sessions s where s.project_id = ?1
             order by coalesce(s.updated_at, s.created_at) desc""",
        ("p",),
    ).fetchone()
    assert isinstance(row[12], int), "turnCount at index 12 is %r" % (row[12],)
    assert isinstance(row[13], str) and row[13] == "write a note", (
        "first_prompt at index 13 is %r" % (row[13],)
    )
    assert isinstance(row[6], str) and isinstance(row[10], str) and isinstance(row[11], str)


def no_turns(c):
    row = c.execute(
        """select s.id,
                  (select count(*) from turns t where t.session_id = s.id),
                  (select coalesce(min(t.prompt_text), '') from turns t
                    where t.session_id = s.id and t.ordinal = 1)
             from sessions s where s.project_id = 'p'"""
    ).fetchone()
    assert row[1] == 0 and row[2] == "", row


def open_turn(c):
    c.execute(
        "insert into turns(session_id,ordinal,prompt_id,status,mode,effort,model,"
        "prompt_text,started_at) values('s',1,'p1','running','default','high','m','hi',"
        "strftime('%Y-%m-%dT%H:%M:%fZ','now'))"
    )
    got = c.execute("select status,prompt_text from turns where session_id='s'").fetchone()
    assert got == ("running", "hi"), got


def close_turn(c):
    c.execute("insert into turns(session_id,ordinal,prompt_id,status) values('s',1,'p1','running')")
    c.execute("update turns set status='done', cost_micros=1234 where session_id='s' and ordinal=1")
    got = c.execute("select status,cost_micros from turns where session_id='s'").fetchone()
    assert got == ("done", 1234), got


def usage(c):
    c.execute(
        "insert into usage(session_id,turn_id,model,input_tokens,cache_write_tokens,"
        "cache_read_tokens,output_tokens,cost_micros) values('s',NULL,'m',1,2,3,4,5)"
    )
    got = c.execute("select sum(cost_micros) from usage").fetchone()[0]
    assert got == 5, got


def settings(c):
    c.execute(
        "insert into settings(project_id,key,value,source,updated_at)"
        " values('p','theme','dark','project',strftime('%Y-%m-%dT%H:%M:%fZ','now'))"
        " on conflict(project_id,key,source) do update set value=excluded.value"
    )
    got = c.execute("select value from settings where project_id='p' and key='theme'").fetchone()
    assert got[0] == "dark", got


def usage_rollup(c):
    c.execute(
        "insert into usage(session_id,turn_id,model,cost_micros,created_at)"
        " values('s',NULL,'m',7,'2026-11-11T03:15:00.000Z')"
    )
    c.execute(
        "insert into usage_hourly(hour,micros,requests)"
        " select substr(created_at,1,13)||':00:00Z', coalesce(sum(cost_micros),0), count(*)"
        "   from usage group by substr(created_at,1,13)"
        " on conflict(hour) do update set micros=excluded.micros, requests=excluded.requests"
    )
    got = c.execute("select hour,micros,requests from usage_hourly").fetchone()
    assert got == ("2026-11-11T03:00:00Z", 7, 1), got


def main():
    for name, fn in [
        ("list_sessions column types", list_sessions),
        ("list_sessions with no turns", no_turns),
        ("open_turn", open_turn),
        ("close_turn", close_turn),
        ("usage insert", usage),
        ("settings upsert", settings),
        ("usage_hourly rollup", usage_rollup),
    ]:
        check(name, fn)
    ok = True
    for name, good, err in results:
        print(("PASS  " if good else "FAIL  ") + name + (("  <- " + err) if err else ""))
        ok = ok and good
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
