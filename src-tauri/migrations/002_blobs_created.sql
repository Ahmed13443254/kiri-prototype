-- 002: file_blobs needs a write time.
--
-- architecture.md's gc job is "drops file_blobs with no file_checkpoints
-- reference and older than 7 days". 001 does not carry a timestamp on
-- file_blobs, so the age half of that rule had no column to read. This adds it
-- with a default that matches the project's one time format; existing rows are
-- stamped by the same default.
--
-- Everything else in 001 is unchanged.

alter table file_blobs
  add column created_at text not null
  default (strftime('%Y-%m-%dT%H:%M:%fZ','now'));

create index file_blobs_created on file_blobs (created_at);
