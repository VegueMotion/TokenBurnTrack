import json, os, sqlite3, sys
from compression import zstd

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "1.29.2", "threads", "threads.db")

DDL = """
CREATE TABLE threads (
    id TEXT PRIMARY KEY,
    summary TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    data_type TEXT NOT NULL,
    data BLOB NOT NULL
, parent_id TEXT, worktree_branch TEXT, folder_paths TEXT, folder_paths_order TEXT, created_at TEXT);
CREATE INDEX threads_list_metadata ON threads (
    updated_at, created_at, id, parent_id, folder_paths, folder_paths_order, summary
);
"""


def usage(i=0, o=0, cw=0, cr=0):
    u = {}
    if i: u["input_tokens"] = i
    if o: u["output_tokens"] = o
    if cw: u["cache_creation_input_tokens"] = cw
    if cr: u["cache_read_input_tokens"] = cr
    return u


def user(mid, text):
    return {"User": {"id": mid, "content": [{"Text": text}]}}


def agent(text):
    return {"Agent": {"content": [{"Text": text}], "tool_results": {}, "reasoning_details": None}}


def thread(title, updated, msgs, total, last, provider, model):
    t = {
        "title": title,
        "messages": msgs,
        "updated_at": updated,
        "detailed_summary": None,
        "initial_project_snapshot": None,
        "cumulative_token_usage": total,
        "request_token_usage": last,
        "model": {"provider": provider, "model": model} if model else None,
        "profile": "write",
        "subagent_context": None,
        "speed": None,
        "thinking_enabled": False,
        "thinking_effort": None,
        "thread_only": False,
        "version": "0.3.0",
    }
    return t


ROWS = [
    dict(
        id="4f6c1d2a-8b3e-4a7f-9c1d-2e3f4a5b6c7d", parent=None,
        paths="/home/user/projects/lorem", order="0",
        created="2026-11-20T09:15:00.000000000+00:00", updated="2026-11-20T09:42:17.123456789+00:00",
        kind="zstd",
        data=thread("Lorem ipsum refactor", "2026-11-20T09:42:17.123456789Z",
                    [user("u-1", "Lorem ipsum dolor sit amet?"), agent("Consectetur adipiscing elit."),
                     user("u-2", "Sed do eiusmod tempor?"), agent("Ut labore et dolore magna aliqua.")],
                    usage(18250, 2140, 9600, 41800), {"u-1": usage(6100, 700, 9600, 0), "u-2": usage(4200, 520, 0, 15700)},
                    "zed.dev", "claude-sonnet-4-5"),
    ),
    dict(
        id="7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d", parent="4f6c1d2a-8b3e-4a7f-9c1d-2e3f4a5b6c7d",
        paths="/home/user/projects/lorem", order="0",
        created="2026-11-20T09:30:02.000000000+00:00", updated="2026-11-20T09:31:45.500000000+00:00",
        kind="zstd",
        data=thread("Ut enim ad minim", "2026-11-20T09:31:45.5Z",
                    [user("u-3", "Quis nostrud exercitation?"), agent("Ullamco laboris nisi.")],
                    usage(3200, 480, 0, 1200), {"u-3": usage(1700, 300, 0, 1200)},
                    "anthropic", "claude-haiku-4-5"),
    ),
    dict(
        id="b1c2d3e4-f5a6-4b7c-8d9e-0f1a2b3c4d5e", parent=None,
        paths="/home/user/projects/dolor\n/home/user/projects/sit", order="1,0",
        created="2026-11-21T14:00:00.000000000+00:00", updated="2026-11-21T14:05:09.000000000+00:00",
        kind="zstd",
        data=thread("Duis aute irure", "2026-11-21T14:05:09Z",
                    [user("u-4", "Reprehenderit in voluptate?"), agent("Velit esse cillum dolore.")],
                    usage(950, 1320), {"u-4": usage(950, 1320)},
                    "lmstudio", "deepseek-r1-distill-qwen-7b"),
    ),
    dict(
        id="c9d8e7f6-a5b4-4c3d-9e2f-1a0b9c8d7e6f", parent=None,
        paths=None, order=None,
        created="2026-11-22T18:20:00.000000000+00:00", updated="2026-11-22T18:24:31.000000000+00:00",
        kind="json",
        data=thread("Excepteur sint", "2026-11-22T18:24:31Z",
                    [user("u-5", "Occaecat cupidatat non proident?"), agent("Sunt in culpa qui officia.")],
                    usage(5000, 800, 0, 2048), {"u-5": usage(5000, 800, 0, 2048)},
                    "openai", "gpt-5"),
    ),
    dict(
        id="d0e1f2a3-b4c5-4d6e-8f7a-9b0c1d2e3f4a", parent=None,
        paths="/home/user/projects/lorem", order="0",
        created="2026-11-23T08:00:00.000000000+00:00", updated="2026-11-23T08:00:00.000000000+00:00",
        kind="zstd",
        data=thread("New Thread", "2026-11-23T08:00:00Z", [], {}, {}, None, None),
    ),
]


def main():
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    if os.path.exists(OUT):
        os.remove(OUT)
    con = sqlite3.connect(OUT)
    con.executescript(DDL)
    for r in ROWS:
        raw = json.dumps(r["data"], separators=(",", ":")).encode()
        blob = zstd.compress(raw, level=3) if r["kind"] == "zstd" else raw
        con.execute(
            "INSERT INTO threads (id, parent_id, folder_paths, folder_paths_order, summary, updated_at, data_type, data, created_at)"
            " VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (r["id"], r["parent"], r["paths"], r["order"], r["data"]["title"], r["updated"], r["kind"], blob, r["created"]),
        )
    con.commit()
    con.execute("VACUUM")
    con.close()


if __name__ == "__main__":
    sys.exit(main())
