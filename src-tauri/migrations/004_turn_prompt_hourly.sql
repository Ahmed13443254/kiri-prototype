-- 004: two columns the store layer already reads/writes.
--
-- 1. turns.prompt_text: history::list_sessions derives a session title from
--    its first prompt (min(prompt_text) where ordinal = 1). 001 carries
--    prompt_id but never the text the user typed, so that query failed with
--    "no such column: t.prompt_text". Nullable: a turn indexed from a
--    transcript written by an older build has no prompt text, and the title
--    falls back to "(untitled)".
--
-- 2. usage_hourly: architecture.md's usage.rollup job writes the cached
--    per-hour totals the cost panel reads. It was described but never
--    declared, so the hourly job failed on every run. It is a derived cache:
--    dropping it loses no accounting, `usage` is the record.

alter table turns add column prompt_text text;

create table if not exists usage_hourly (
  hour     text primary key,
  micros   integer not null default 0,
  requests integer not null default 0
);
