import calendar, json, os, sqlite3, sys, time

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "1.61.0", "sessions", "sessions.db")

DDL = """
CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    user_set_name BOOLEAN DEFAULT FALSE,
    session_type TEXT NOT NULL DEFAULT 'user',
    working_dir TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    extension_data TEXT DEFAULT '{}',
    total_tokens INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    accumulated_total_tokens INTEGER,
    accumulated_input_tokens INTEGER,
    accumulated_output_tokens INTEGER,
    accumulated_cache_read_tokens INTEGER,
    accumulated_cache_write_tokens INTEGER,
    accumulated_cost REAL,
    schedule_id TEXT,
    recipe_json TEXT,
    user_recipe_values_json TEXT,
    provider_name TEXT,
    model_config_json TEXT,
    goose_mode TEXT NOT NULL DEFAULT 'auto',
    archived_at TIMESTAMP,
    project_id TEXT,
    parent_session_id TEXT,
    system_prompt_override TEXT,
    system_prompt_extras_json TEXT,
    container_id TEXT
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role TEXT NOT NULL,
    content_json TEXT NOT NULL,
    created_timestamp INTEGER NOT NULL,
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    tokens INTEGER,
    metadata_json TEXT
);
CREATE TABLE usage_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    created_timestamp INTEGER NOT NULL,
    model TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    total_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    cost REAL,
    cost_source TEXT,
    is_compaction INTEGER DEFAULT 0
);
CREATE INDEX idx_messages_session ON messages(session_id);
CREATE INDEX idx_messages_timestamp ON messages(timestamp);
CREATE INDEX idx_messages_message_id ON messages(message_id);
CREATE INDEX idx_messages_session_created ON messages(session_id, created_timestamp, id);
CREATE INDEX idx_sessions_updated ON sessions(updated_at DESC);
CREATE INDEX idx_sessions_type ON sessions(session_type);
CREATE INDEX idx_sessions_parent ON sessions(parent_session_id);
CREATE INDEX idx_usage_ledger_session ON usage_ledger(session_id);
"""


def model(name):
    return json.dumps({"model_name": name, "temperature": None, "max_tokens": None,
                       "toolshim": False, "toolshim_model": None})


# (id, name, working_dir, created, updated, provider, model, parent, accumulated in/out/cache_read/cache_write)
SESSIONS = [
    ("20261120_1", "Lorem ipsum refactor", "/home/user/projects/lorem",
     "2026-11-20 09:15:00", "2026-11-20 09:42:17", "anthropic", "claude-sonnet-4-5", None,
     (64000, 2900, 41800, 9600)),
    ("20261120_2", "Ut enim ad minim", "/home/user/projects/lorem",
     "2026-11-20 09:30:02", "2026-11-20 09:31:45", "ollama", "qwen3-coder:30b", "20261120_1",
     (2200, 310, 0, 0)),
    ("20261018_3", "Duis aute irure", "/home/user/projects/dolor",
     "2026-10-18 14:00:00", "2026-10-18 14:05:09", "openai", "gpt-4.1", None,
     (12000, 900, 4096, 0)),
    ("20261021_1", "Excepteur sint", "/home/user/projects/sit",
     "2026-10-21 08:00:00", "2026-11-22 18:24:31", "openai", "gpt-4.1", None,
     (9000, 700, 2048, 0)),
    ("20261122_2", "Claude Code relay", "/home/user/projects/lorem",
     "2026-11-22 19:00:00", "2026-11-22 19:03:00", "claude-code", "default", None,
     (5000, 400, 3000, 0)),
    ("20261123_1", "Sunt in culpa", "/home/user/projects/amet",
     "2026-11-23 10:00:00", "2026-11-23 10:02:00", "openrouter", "z-ai/glm-4.6", None,
     (3000, 600, 0, 0)),
    ("20261124_1", "New session", "/home/user",
     "2026-11-24 08:00:00", "2026-11-24 08:00:00", None, None, None,
     (0, 0, 0, 0)),
]

# (session, UTC time, model, in, out, cache_read, cache_write, cost, cost_source, is_compaction)
LEDGER = [
    ("20261120_1", "2026-11-20 09:20:05", "claude-sonnet-4-5-20250929", 30000, 1200, 15700, 9600, 0.0617, "estimated", 0),
    ("20261120_2", "2026-11-20 09:31:42", "qwen3-coder:30b", 2200, 310, 0, 0, None, None, 0),
    ("20261120_1", "2026-11-20 09:42:17", "claude-sonnet-4-5-20250929", 34000, 1700, 26100, 0, 0.0442, "estimated", 1),
    ("20261021_1", "2026-11-22 18:24:00", None, 6000, 500, 2048, 0, None, "carried_forward", 0),
    ("20261021_1", "2026-11-22 18:24:31", "gpt-4.1-2025-04-14", 3000, 200, 0, 0, 0.0076, "estimated", 0),
    ("20261122_2", "2026-11-22 19:03:00", "claude-sonnet-4-5", 5000, 400, 3000, 0, 0.0057, "provider_reported", 0),
    ("20261123_1", "2026-11-23 10:02:00", "z-ai/glm-4.6", 3000, 600, 0, 0, 0.00231, "provider_reported", 0),
]


def main():
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    if os.path.exists(OUT):
        os.remove(OUT)
    con = sqlite3.connect(OUT)
    con.executescript(DDL)
    con.execute("INSERT INTO schema_version (version, applied_at) VALUES (18, '2026-10-01 00:00:00')")
    for sid, name, wd, created, updated, provider, m, parent, (i, o, cr, cw) in SESSIONS:
        con.execute(
            "INSERT INTO sessions (id, name, working_dir, created_at, updated_at, provider_name,"
            " model_config_json, parent_session_id, accumulated_input_tokens, accumulated_output_tokens,"
            " accumulated_cache_read_tokens, accumulated_cache_write_tokens, accumulated_total_tokens)"
            " VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (sid, name, wd, created, updated, provider, model(m) if m else None, parent,
             i or None, o or None, cr or None, cw or None, (i + o) or None),
        )
    for sid, ts, m, i, o, cr, cw, cost, source, compaction in LEDGER:
        con.execute(
            "INSERT INTO usage_ledger (session_id, created_timestamp, model, input_tokens, output_tokens,"
            " total_tokens, cache_read_tokens, cache_write_tokens, cost, cost_source, is_compaction)"
            " VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (sid, calendar.timegm(time.strptime(ts, "%Y-%m-%d %H:%M:%S")), m, i, o, i + o, cr, cw, cost, source, compaction),
        )
    con.commit()
    con.execute("VACUUM")
    con.close()


if __name__ == "__main__":
    sys.exit(main())
