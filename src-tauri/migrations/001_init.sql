-- kiri engine schema. SQLite 3.45+, WAL.
-- Contract: replica/recon.md. Derived index over .jsonl transcripts.
-- Times: text, ISO-8601 UTC, 'Z'. Money: integer micro-USD.
-- Access: src-tauri/src/db/ owns every query; scope is checked there.

pragma journal_mode = wal;
pragma foreign_keys = on;
pragma synchronous = normal;
pragma busy_timeout = 5000;

create table app_meta (
  version    integer not null,
  applied_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

create table projects (
  id             text primary key,
  path           text not null unique,
  name           text not null,
  git_remote     text,
  created_at     text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at     text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  last_opened_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
create index projects_last_opened on projects (last_opened_at desc);

create table workspaces (
  id         text primary key,
  project_id text not null references projects(id) on delete cascade,
  path       text not null unique,
  branch     text,
  base_ref   text,
  kind       text not null default 'primary'
             check (kind in ('primary','worktree')),
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  removed_at text
);
create index workspaces_project on workspaces (project_id);

create table sessions (
  id              text primary key,
  project_id      text not null references projects(id) on delete cascade,
  workspace_id    text references workspaces(id) on delete set null,
  title           text,
  model           text not null,
  effort          text check (effort in ('low','medium','high','xhigh','max')),
  permission_mode text not null default 'default'
                  check (permission_mode in
                    ('default','dontAsk','acceptEdits','bypassPermissions','plan','auto')),
  status          text not null default 'active'
                  check (status in ('active','idle','interrupted','done','error')),
  transcript_path text not null,
  pid             integer,
  fork_of         text references sessions(id) on delete set null,
  cost_micros     integer not null default 0,
  created_at      text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at      text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ended_at        text
);
create index sessions_project on sessions (project_id, created_at desc);
create index sessions_status on sessions (status) where ended_at is null;
create index sessions_fork on sessions (fork_of);

create table turns (
  id          integer primary key autoincrement,
  session_id  text not null references sessions(id) on delete cascade,
  ordinal     integer not null,
  prompt_id   text,
  status      text not null default 'pending'
              check (status in ('pending','running','done','interrupted','error')),
  mode        text,
  effort      text,
  model       text,
  cost_micros integer not null default 0,
  started_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ended_at    text,
  unique (session_id, ordinal)
);
create index turns_session on turns (session_id, ordinal);

create table messages (
  id           text primary key,
  session_id   text not null references sessions(id) on delete cascade,
  turn_id      integer references turns(id) on delete set null,
  parent_uuid  text,
  role         text not null check (role in ('user','assistant','system')),
  message_id   text,
  is_sidechain integer not null default 0,
  content      text not null,
  created_at   text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
create index messages_session on messages (session_id, created_at);
create index messages_turn on messages (turn_id);
create index messages_parent on messages (parent_uuid);
create index messages_msgid on messages (message_id);

create table blocks (
  id          integer primary key autoincrement,
  message_id  text not null references messages(id) on delete cascade,
  idx         integer not null,
  kind        text not null
              check (kind in ('text','thinking','tool_use','tool_result','image','other')),
  text        text,
  tool_use_id text,
  is_error    integer not null default 0,
  json        text,
  unique (message_id, idx)
);
create index blocks_tool on blocks (tool_use_id);
create index blocks_kind on blocks (kind);

create table tool_calls (
  id                 text primary key,
  session_id         text not null references sessions(id) on delete cascade,
  turn_id            integer references turns(id) on delete set null,
  parent_tool_use_id text,
  agent_id           text,
  name               text not null,
  input              text,
  result             text,
  status             text not null default 'running'
                     check (status in
                       ('pending','running','done','error','denied','stopped')),
  duration_ms        integer,
  created_at         text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ended_at           text
);
create index tool_calls_session on tool_calls (session_id, created_at);
create index tool_calls_turn on tool_calls (turn_id);
create index tool_calls_parent on tool_calls (parent_tool_use_id);
create table permission_requests (
  id             text primary key,
  request_id     text not null unique,
  session_id     text not null references sessions(id) on delete cascade,
  turn_id        integer references turns(id) on delete set null,
  tool_call_id   text not null references tool_calls(id) on delete cascade,
  tool_name      text not null,
  input          text not null,
  decision_reason text,
  blocked_path   text,
  suggestions    text,
  default_no     integer not null default 0,
  agent_id       text,
  status         text not null default 'pending'
                 check (status in ('pending','allowed','denied','always_allow','cancelled')),
  created_at     text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  resolved_at    text
);
create index perm_pending on permission_requests (session_id, status) where status = 'pending';

create table permission_rules (
  id          text primary key,
  project_id  text not null references projects(id) on delete cascade,
  rule_type   text not null check (rule_type in ('allow','deny','ask')),
  tool        text not null,
  pattern     text,
  scope       text not null default 'local'
              check (scope in ('session','local','project','user','managed')),
  origin      text not null default 'user'
              check (origin in ('user','always_allow','policy_file','import')),
  enabled     integer not null default 1,
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  unique (project_id, rule_type, tool, pattern, scope)
);
create index permission_rules_lookup on permission_rules (project_id, tool, rule_type)
  where enabled = 1;

create table questions (
  id         text primary key,
  request_id text not null unique,
  session_id text not null references sessions(id) on delete cascade,
  turn_id    integer references turns(id) on delete set null,
  tool_name  text not null default 'AskUserQuestion',
  payload    text not null,
  answers    text,
  status     text not null default 'pending'
             check (status in ('pending','answered','timeout','cancelled')),
  timeout_s  integer not null default 300,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  answered_at text
);
create index questions_pending on questions (session_id, status) where status = 'pending';

create table tasks (
  id          text primary key,
  session_id  text not null references sessions(id) on delete cascade,
  tool_call_id text references tool_calls(id) on delete set null,
  kind        text not null check (kind in ('bash','agent','monitor','cron','workflow')),
  subject     text not null,
  description text,
  active_form text,
  status      text not null default 'pending'
              check (status in ('pending','in_progress','completed','deleted','failed','stopped')),
  owner       text,
  subagent_id text,
  blocks      text not null default '[]',
  blocked_by  text not null default '[]',
  metadata    text not null default '{}',
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
create index tasks_session on tasks (session_id, status);
create index tasks_call on tasks (tool_call_id);

create table file_blobs (
  sha       text primary key,           -- sha256 of the body
  size      integer not null,
  binary    integer not null default 0,
  body      text
);

create table file_checkpoints (
  id          text primary key,
  session_id  text not null references sessions(id) on delete cascade,
  turn_id     integer references turns(id) on delete set null,
  path        text not null,
  blob_sha    text references file_blobs(sha) on delete set null,
  existed_pre integer not null default 1,
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
create index checkpoints_session on file_checkpoints (session_id, created_at);
create index checkpoints_path on file_checkpoints (session_id, path);

create table hooks (
  id          text primary key,
  scope       text not null default 'project-user',
  event       text not null,
  matcher     text,
  hook_type   text not null check (hook_type in ('command','prompt','http')),
  command     text,
  url         text,
  body_json   text,
  timeout_s   integer not null default 60,
  enabled     integer not null default 1,
  source_path text,
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  check (hook_type <> 'http' or url is not null)
);
create index hooks_event on hooks (event, enabled);

create table settings (
  project_id text not null references projects(id) on delete cascade,
  key        text not null,
  value      text not null,
  source     text not null default 'user'
             check (source in ('user','project','local','managed')),
  file_path  text,
  updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  primary key (project_id, key, source)

);
create index settings_key on settings (key);

create table plugins (
  id          text primary key,
  project_id  text references projects(id) on delete cascade,
  name        text not null,
  version     text,
  marketplace text,
  path        text not null,
  enabled     integer not null default 0,
  manifest    text,
  installed_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  unique (project_id, name)
);
create index plugins_enabled on plugins (enabled);

create table skills (
  id          text primary key,
  project_id  text references projects(id) on delete cascade,
  name        text not null,
  description text,
  body        text not null,
  frontmatter text not null default '{}',
  allowed_tools text,
  source_path text,
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  updated_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  unique (project_id, name)
);

create table mcp_servers (
  id          text primary key,
  project_id  text references projects(id) on delete cascade,
  name        text not null,
  transport   text not null check (transport in ('stdio','http','sse')),
  command     text,
  url         text,
  env         text not null default '{}',
  headers     text not null default '{}',
  scope       text not null default 'local' check (scope in ('local','user','project')),
  status      text not null default 'disconnected'
              check (status in ('disconnected','connecting','connected','error')),
  last_error  text,
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  unique (project_id, name),
  check (transport <> 'stdio' or command is not null),
  check (transport = 'stdio' or url is not null)
);
create index mcp_servers_status on mcp_servers (status);
create index mcp_servers_global on mcp_servers (name) where project_id is null;

create table mcp_tools (
  id          text primary key,
  server_id   text not null references mcp_servers(id) on delete cascade,
  name        text not null,
  description text,
  input_schema text,
  loaded      integer not null default 0,
  unique (server_id, name)
);
create index mcp_tools_loaded on mcp_tools (loaded);

create table subagents (
  id          text primary key,
  session_id  text not null references sessions(id) on delete cascade,
  parent_call_id text,
  agent_type  text not null,
  agent_name  text,
  prompt      text not null,
  model       text,
  isolation   text check (isolation in ('worktree','remote')),
  status      text not null default 'running'
              check (status in ('running','done','error','stopped')),
  result      text,
  worktree    text,
  cost_micros integer not null default 0,
  created_at  text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ended_at    text
);
create index subagents_session on subagents (session_id, created_at);
create index subagents_parent on subagents (parent_call_id);

create table cron_jobs (
  id         text primary key,
  session_id text not null references sessions(id) on delete cascade,
  cron       text not null,
  prompt     text not null,
  recurring  integer not null default 1,
  durable    integer not null default 0,
  enabled    integer not null default 1,
  last_run   text,
  next_run   text,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
create index cron_next on cron_jobs (enabled, next_run);

create table wakeups (
  id         text primary key,
  session_id text not null references sessions(id) on delete cascade,
  fire_at    text not null,
  reason     text,
  prompt     text,
  sent_at    text
);
create index wakeups_due on wakeups (fire_at) where sent_at is null;

create table usage (
  id         integer primary key autoincrement,
  session_id text references sessions(id) on delete set null,
  turn_id    integer references turns(id) on delete set null,
  model      text not null,
  input_tokens integer not null default 0,
  cache_write_tokens integer not null default 0,
  cache_read_tokens integer not null default 0,
  output_tokens integer not null default 0,
  cost_micros integer not null default 0,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
create index usage_session on usage (session_id, created_at);
create index usage_created on usage (created_at);
