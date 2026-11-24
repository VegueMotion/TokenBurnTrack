mod common;
mod e2e_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use e2e_support::*;
use rusqlite::params;
use serde_json::{Value, json};
use tbt_collector::RunTrigger;
use tbt_collector::recompute::recompute_for_price;
use tbt_core::{Micros, ModelPrice, PriceSource, Timestamp};
use tbt_store::Store;

const CODEX_THREAD: &str = "01a0b44c-76aa-76b8-86dd-e11f3ffd813c";
const CODEX_CONTINUATION: &str = "rollout-2026-09-18T15-31-24-01a0b44c-76aa-76b8-86dd-e11f3ffd813c_01a0b4b6-f515-7c6f-8a5f-1faf9245c651.jsonl";
const CODEX_SHORT: &str = "rollout-2026-05-31T19-42-32-019e7f21-575c-7721-8bee-e12a2dcde581.jsonl";
const OPENCODE_SESSION: &str = "ses_e20abeb75ffeKnigVjBVuBB9rl";
const GEMINI_MAIN: &str = "session-2026-11-20T10-15-5a1e2d3c.jsonl";
const GEMINI_RESUMED: &str = "session-2026-10-02T09-30-7a1b2c3d.jsonl";
const PROXY_DAY: &str = "2026-11-22.jsonl";
const CLAUDE_RESUME_ID: &str = "e2e0e2e0-0000-4000-8000-00000000c1a0";
const OPUS: &str = "claude-opus-5-5";

fn opencode_db(w: &World) -> PathBuf {
    w.root("opencode/1.19.5").join("opencode.db")
}

#[test]
fn backfill_then_hourly_reruns_with_the_builtin_registry() {
    let w = World::new();
    let collector = w.builtin();
    assert_eq!(
        collector.registry().ids().len(),
        BUILTIN_SETS.len() + GENERIC_SETS.len()
    );

    let first = run_as(&collector, RunTrigger::Startup);
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert_eq!(first.sources_scanned, SOURCES);
    assert_eq!(first.sources_read, SOURCES);
    assert_eq!(first.sources_failed, 0);
    assert_eq!(first.events_new, GOLDEN_TOTAL);
    assert_eq!(w.per_harness(), golden());
    assert_eq!(warning_set(&first), known_warnings());
    w.assert_consistent();
    assert!(w.env.count("SELECT count(*) FROM model_price") > 0);
    assert_eq!(
        w.env
            .count("SELECT count(*) FROM usage_event WHERE price_id IS NULL AND harness_id IN ('claude_code', 'codex', 'copilot_cli', 'cursor', 'gemini_cli')"),
        0,
        "every built-in harness model except local OpenCode ones has a bundled price"
    );

    let before = totals(&w.db());
    for _ in 0..3 {
        assert_quiet_rerun(&run_as(&collector, RunTrigger::Hourly));
    }
    assert_eq!(totals(&w.db()), before);
    w.assert_consistent();

    let runs = w.env.store().ingest_runs(10).unwrap();
    assert_eq!(runs.len(), 4);
    assert_eq!(runs[3].stats.events_new, GOLDEN_TOTAL);
    assert_eq!(runs[3].warnings.len(), KNOWN_WARNINGS.len());
    assert!(
        runs[..3]
            .iter()
            .all(|r| r.stats.events_new == 0 && r.warnings.is_empty() && r.error.is_none())
    );

    let status = w.env.store().harness_status().unwrap();
    assert_eq!(status.iter().map(|h| h.files_tracked).sum::<u64>(), SOURCES);
    assert!(status.iter().all(|h| h.files_missing == 0));
}

fn rewrite_session_id(v: &mut Value, new_id: &str) {
    if let Some(obj) = v.as_object_mut() {
        for key in ["sessionId", "session_id"] {
            if obj.contains_key(key) {
                obj.insert(key.into(), Value::String(new_id.into()));
            }
        }
    }
}

fn last_claude_call(text: &str) -> Value {
    text.lines()
        .rev()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["type"] == "assistant" && v["message"]["usage"].is_object())
        .unwrap()
}

fn claude_call(template: &Value, session: &str, n: usize, at: &str, parent: &Value) -> Value {
    let mut call = template.clone();
    rewrite_session_id(&mut call, session);
    call["uuid"] = json!(format!("e2e0e2e0-0000-4000-8000-{n:012}"));
    call["parentUuid"] = parent.clone();
    call["timestamp"] = json!(at);
    call["requestId"] = json!(format!("req_e2e{n:021}"));
    call["message"]["id"] = json!(format!("msg_e2e{n:021}"));
    call["message"]["model"] = json!(OPUS);
    let usage = &mut call["message"]["usage"];
    usage["input_tokens"] = json!(10 + n);
    usage["output_tokens"] = json!(500 * (n + 1));
    usage["cache_read_input_tokens"] = json!(20_000);
    usage["cache_creation_input_tokens"] = json!(1_000);
    usage["cache_creation"] = json!({
        "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 1_000
    });
    call
}

fn claude_resume(w: &World, original: &str, new_id: &str, calls: &[&str]) -> PathBuf {
    let path = w.env.find(&format!("{original}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap();
    let template = last_claude_call(&text);
    let mut lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for line in &mut lines {
        rewrite_session_id(line, new_id);
    }
    lines.insert(
        0,
        json!({
            "type": "derived-from", "v": 1, "sessionId": new_id,
            "sessions": [{"type": "claude_code_session", "id": original, "via": "fork"}]
        }),
    );
    let mut parent = template["uuid"].clone();
    for (i, at) in calls.iter().enumerate() {
        let call = claude_call(&template, new_id, i, at, &parent);
        parent = call["uuid"].clone();
        lines.push(call);
    }
    let out = path.with_file_name(format!("{new_id}.jsonl"));
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&out, body).unwrap();
    out
}

fn codex_turn(ordinal: u64, at: &str, response: &str, usage: [u64; 4], total: u64) -> String {
    let [input, cached, output, reasoning] = usage;
    let usage = json!({
        "input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
        "output_tokens": output, "reasoning_output_tokens": reasoning,
        "total_tokens": input + output
    });
    let thread = json!({
        "input_tokens": total, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
        "output_tokens": 0, "reasoning_output_tokens": 0, "total_tokens": total
    });
    let lines = [
        json!({"timestamp": at, "ordinal": ordinal, "type": "turn_context", "payload": {
            "turn_id": format!("turn-{response}"), "cwd": "/u/user/user/projects/pale-fjord-8a",
            "model": "gpt-6-astra"
        }}),
        json!({"timestamp": at, "ordinal": ordinal + 1, "type": "token_usage_record", "payload": {
            "thread_id": CODEX_THREAD, "turn_id": format!("turn-{response}"),
            "session_id": CODEX_THREAD, "response_id": response, "usage": usage,
            "turn_token_usage": usage, "thread_token_usage": thread
        }}),
        json!({"timestamp": at, "ordinal": ordinal + 2, "type": "event_msg", "payload": {
            "type": "token_count",
            "info": {"total_token_usage": thread, "last_token_usage": usage}
        }}),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn codex_resume_file(w: &World) -> PathBuf {
    let continuation = w.env.find(CODEX_CONTINUATION);
    let text = std::fs::read_to_string(&continuation).unwrap();
    let mut meta: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    meta["timestamp"] = json!("2026-11-24T08:00:00.000Z");
    meta["ordinal"] = json!(258);
    meta["payload"]["history_base"]["end_ordinal_exclusive"] = json!(258);
    meta["payload"]["history_base"]["end_byte_offset"] = json!(text.len());
    let dir = w.root("codex").join("0.155.0/sessions/2026/11/24");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "rollout-2026-11-24T08-00-00-{CODEX_THREAD}_01a0e2e0-0000-7000-8000-000000000001.jsonl"
    ));
    let body = format!(
        "{meta}\n{}",
        codex_turn(
            259,
            "2026-11-24T08:01:00.000Z",
            "resp_e2e_resume_1",
            [9_000, 8_000, 300, 20],
            900_000
        )
    );
    std::fs::write(&path, body).unwrap();
    path
}

fn opencode_message(created: i64, completed: Option<i64>, tokens: [u64; 3]) -> String {
    let [input, output, read] = tokens;
    let mut time = json!({"created": created});
    if let Some(done) = completed {
        time["completed"] = json!(done);
    }
    json!({
        "parentID": "msg_1df5414e10019ErPIGyaOp5PBg", "role": "assistant", "mode": "build",
        "agent": "build",
        "path": {"cwd": "/mnt/bold-juniper-8a/projects/keen-meadow-d6",
                 "root": "/mnt/bold-juniper-8a/projects/keen-meadow-d6"},
        "cost": 0,
        "tokens": {"total": input + output + read, "input": input, "output": output,
                   "reasoning": 0, "cache": {"write": 0, "read": read}},
        "modelID": "qwen3.5:9b", "providerID": "ollama", "time": time
    })
    .to_string()
}

fn opencode_writer(db: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn
}

fn opencode_insert(
    conn: &rusqlite::Connection,
    id: &str,
    at: i64,
    completed: bool,
    tokens: [u64; 3],
) {
    let data = opencode_message(at - 5_000, completed.then_some(at), tokens);
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, OPENCODE_SESSION, at - 5_000, at, data],
    )
    .unwrap();
    let parts = [
        json!({"type": "step-start"}),
        json!({"type": "text", "text": "lorem ipsum dolor"}),
    ];
    for (i, part) in parts.iter().enumerate() {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![
                format!("prt_{id}_{i}"),
                id,
                OPENCODE_SESSION,
                at,
                part.to_string()
            ],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE session SET time_updated = ?2 WHERE id = ?1",
        params![OPENCODE_SESSION, at],
    )
    .unwrap();
}

fn opencode_complete(conn: &rusqlite::Connection, id: &str, at: i64, tokens: [u64; 3]) {
    let data = opencode_message(at - 60_000, Some(at), tokens);
    conn.execute(
        "UPDATE message SET data = ?2, time_updated = ?3 WHERE id = ?1",
        params![id, data, at],
    )
    .unwrap();
    let finish = json!({
        "type": "step-finish", "reason": "stop",
        "tokens": {"total": tokens.iter().sum::<u64>(), "input": tokens[0], "output": tokens[1],
                   "reasoning": 0, "cache": {"write": 0, "read": tokens[2]}},
        "cost": 0
    });
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        params![
            format!("prt_{id}_finish"),
            id,
            OPENCODE_SESSION,
            at,
            finish.to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE session SET time_updated = ?2 WHERE id = ?1",
        params![OPENCODE_SESSION, at],
    )
    .unwrap();
}

fn gemini_append(w: &World) -> PathBuf {
    let path = w.env.find(GEMINI_MAIN);
    let lines = [
        json!({"id": "e2e0e2e0-0000-4000-8000-0000000a0001", "timestamp": "2026-11-24T09:00:00.000Z",
               "type": "user", "content": [{"text": "lorem ipsum"}]}),
        json!({"$set": {"lastUpdated": "2026-11-24T09:00:00.001Z"}}),
        json!({"id": "e2e0e2e0-0000-4000-8000-0000000a0002", "timestamp": "2026-11-24T09:00:05.000Z",
               "type": "gemini", "content": "dolor sit amet", "model": "gemini-2.5-pro",
               "thoughts": [],
               "tokens": {"input": 5_000, "output": 120, "cached": 1_000, "thoughts": 10,
                          "tool": 0, "total": 5_120}}),
        json!({"$set": {"lastUpdated": "2026-11-24T09:00:05.001Z"}}),
    ];
    append(
        &path,
        &lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    );
    path
}

fn event_cost_by_hour(w: &World, session: &str) -> BTreeMap<String, (i64, i64)> {
    let db = w.db();
    let mut stmt = db
        .prepare(
            "SELECT substr(e.ts, 1, 13), count(*), sum(e.cost_micros) FROM usage_event e
             JOIN session s ON s.id = e.session_id WHERE s.ext_id = ?1 GROUP BY 1",
        )
        .unwrap();
    stmt.query_map([session], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn hourly_claude(w: &World, hour: &str) -> (i64, i64) {
    w.db()
        .query_row(
            "SELECT coalesce(sum(events), 0), coalesce(sum(cost_micros), 0) FROM hourly_usage
             WHERE harness_id = 'claude_code' AND substr(hour, 1, 13) = ?1",
            [hour],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn delta(after: &BTreeMap<String, i64>, before: &BTreeMap<String, i64>) -> BTreeMap<String, i64> {
    after
        .iter()
        .map(|(h, n)| (h.clone(), n - before.get(h).copied().unwrap_or(0)))
        .filter(|(_, d)| *d != 0)
        .collect()
}

fn expected(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
    pairs.iter().map(|(h, n)| (h.to_string(), *n)).collect()
}

#[test]
fn resumed_sessions_in_every_harness_are_never_double_counted() {
    let w = World::new();
    let collector = w.clocked();
    let first = run_as(&collector, RunTrigger::Startup);
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert_eq!(w.per_harness(), golden());
    let base = w.per_harness();
    let claude_original = w.session_row("claude_code", LONG_SESSION);
    let codex_before = w.session_row("codex", CODEX_THREAD);

    let claude_calls = [
        "2026-11-24T05:10:00.000Z",
        "2026-11-24T05:50:00.000Z",
        "2026-11-24T06:15:00.000Z",
    ];
    claude_resume(&w, LONG_SESSION, CLAUDE_RESUME_ID, &claude_calls);

    append(
        &w.env.find(CODEX_CONTINUATION),
        &(codex_turn(
            258,
            "2026-11-24T07:00:00.000Z",
            "resp_e2e_append_1",
            [40_000, 39_000, 150, 10],
            873_664,
        ) + &codex_turn(
            261,
            "2026-11-24T07:00:30.000Z",
            "resp_e2e_append_2",
            [41_000, 40_000, 60, 0],
            914_724,
        )),
    );
    codex_resume_file(&w);

    let now = Timestamp::now().as_millisecond();
    let oc = opencode_writer(&opencode_db(&w));
    let db_file = std::fs::metadata(opencode_db(&w)).unwrap();
    opencode_insert(
        &oc,
        "msg_e2e00000001AAAAAAAAAAAAAA",
        now - 60_000,
        true,
        [1_000, 200, 500],
    );
    opencode_insert(
        &oc,
        "msg_e2e00000002AAAAAAAAAAAAAA",
        now - 30_000,
        false,
        [800, 50, 0],
    );
    let db_after = std::fs::metadata(opencode_db(&w)).unwrap();
    assert_eq!(
        (db_after.len(), db_after.modified().unwrap()),
        (db_file.len(), db_file.modified().unwrap()),
        "the new OpenCode rows sit in the -wal file only"
    );

    let gemini_file = gemini_append(&w);
    w.set_clock(mtime(&gemini_file) + jiff::SignedDuration::from_secs(60));

    let second = run_as(&collector, RunTrigger::Hourly);
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(
        delta(&w.per_harness(), &base),
        expected(&[("claude_code", 3), ("codex", 3), ("opencode", 1)])
    );
    assert_eq!(second.events_new, 7);

    assert_eq!(
        w.env
            .session_link(CLAUDE_RESUME_ID, "resumed_from_session_id")
            .as_deref(),
        Some(LONG_SESSION)
    );
    assert_eq!(w.session_row("claude_code", LONG_SESSION), claude_original);
    let resumed = w.session_row("claude_code", CLAUDE_RESUME_ID);
    assert_eq!(resumed.sums[0], 3);
    assert!(resumed.sums[5] > 0, "new Claude calls are priced");
    assert!(
        resumed.started_at.starts_with("2026-11-24T05:10:00"),
        "{}",
        resumed.started_at
    );
    let by_hour = event_cost_by_hour(&w, CLAUDE_RESUME_ID);
    assert_eq!(
        by_hour.keys().cloned().collect::<Vec<_>>(),
        ["2026-11-24T05", "2026-11-24T06"]
    );
    for (hour, (events, cost)) in &by_hour {
        assert_eq!(hourly_claude(&w, hour), (*events, *cost), "{hour}");
    }
    assert_eq!(by_hour["2026-11-24T05"].0, 2);

    assert_eq!(
        w.env.count(
            "SELECT count(*) FROM usage_event WHERE harness_id = 'codex' AND dedup_key LIKE 'resp_e2e_%'"
        ),
        3
    );
    let codex_after = w.session_row("codex", CODEX_THREAD);
    assert_eq!(codex_after.sums[0], codex_before.sums[0] + 3);
    assert_eq!(
        codex_after.sums[1],
        codex_before.sums[1] + 1_000 + 1_000 + 1_000
    );
    assert_eq!(
        codex_after.sums[4],
        codex_before.sums[4] + 39_000 + 40_000 + 8_000
    );
    assert_eq!(
        w.env.session_link(CODEX_THREAD, "resumed_from_session_id"),
        None
    );

    assert_eq!(
        w.env.count(
            "SELECT count(*) FROM usage_event WHERE dedup_key = 'msg_e2e00000002AAAAAAAAAAAAAA'"
        ),
        0,
        "an incomplete OpenCode message is not counted yet"
    );

    w.assert_consistent();

    let quiet = run_as(&collector, RunTrigger::Hourly);
    assert_eq!(quiet.events_new, 0);

    opencode_complete(
        &oc,
        "msg_e2e00000002AAAAAAAAAAAAAA",
        Timestamp::now().as_millisecond(),
        [900, 120, 0],
    );
    w.set_clock(mtime(&gemini_file) + jiff::SignedDuration::from_mins(11));
    let third = run_as(&collector, RunTrigger::Hourly);
    assert!(third.errors.is_empty(), "{:?}", third.errors);
    assert_eq!(third.events_new, 2);
    assert_eq!(
        delta(&w.per_harness(), &base),
        expected(&[
            ("claude_code", 3),
            ("codex", 3),
            ("gemini_cli", 1),
            ("opencode", 2)
        ])
    );
    let (input, output): (i64, i64) = w
        .db()
        .query_row(
            "SELECT input_tokens, output_tokens FROM usage_event
             WHERE dedup_key = 'msg_e2e00000002AAAAAAAAAAAAAA'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((input, output), (900, 120));
    let (input, cache_read): (i64, i64) = w
        .db()
        .query_row(
            "SELECT input_tokens, cache_read_tokens FROM usage_event
             WHERE harness_id = 'gemini_cli' AND ts LIKE '2026-11-24T09:00:05%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((input, cache_read), (4_000, 1_000));

    w.assert_consistent();
    let settled = totals(&w.db());
    for _ in 0..4 {
        let r = run_as(&collector, RunTrigger::Hourly);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.events_new, 0);
        assert_eq!(totals(&w.db()), settled);
    }
    w.set_clock(Timestamp::now() + jiff::SignedDuration::from_hours(48));
    assert_eq!(run_as(&collector, RunTrigger::Hourly).events_new, 0);
    assert_eq!(totals(&w.db()), settled);
    w.assert_consistent();
}

#[test]
fn deleted_sources_are_marked_missing_and_restoring_them_adds_nothing() {
    let w = World::new();
    let collector = w.builtin();
    assert_eq!(
        run_as(&collector, RunTrigger::Startup).events_new,
        GOLDEN_TOTAL
    );
    let before = totals(&w.db());

    let victims: Vec<PathBuf> = vec![
        w.env.find(&format!("{SHORT_SESSION}.jsonl")),
        w.env.find(CODEX_SHORT),
        opencode_db(&w),
        w.root("gemini_cli/0.71.0-nightly")
            .join("tmp/bold-birch-00/chats")
            .join(GEMINI_RESUMED),
        w.env.find(PROXY_DAY),
    ];
    let kept: Vec<Vec<u8>> = victims.iter().map(|p| std::fs::read(p).unwrap()).collect();
    for p in &victims {
        std::fs::remove_file(p).unwrap();
    }

    let gone = run_as(&collector, RunTrigger::Hourly);
    assert!(gone.errors.is_empty(), "{:?}", gone.errors);
    assert_eq!(gone.sources_missing, victims.len() as u64);
    assert_eq!(gone.sources_scanned, SOURCES - victims.len() as u64);
    assert_eq!(gone.events_new, 0);
    assert_eq!(
        w.env
            .count("SELECT count(*) FROM source_file WHERE missing = 1"),
        victims.len() as i64
    );
    assert_eq!(totals(&w.db()), before);
    w.assert_consistent();
    let status = w.env.store().harness_status().unwrap();
    assert_eq!(
        status.iter().map(|h| h.files_missing).sum::<u64>(),
        victims.len() as u64
    );

    let still = run_as(&collector, RunTrigger::Hourly);
    assert_eq!((still.sources_missing, still.events_new), (0, 0));
    assert_eq!(
        w.env
            .count("SELECT count(*) FROM source_file WHERE missing = 1"),
        victims.len() as i64
    );

    for (p, bytes) in victims.iter().zip(&kept) {
        std::fs::write(p, bytes).unwrap();
        set_mtime(
            p,
            std::time::SystemTime::now() - Duration::from_secs(86_400),
        );
    }
    let back = run_as(&collector, RunTrigger::Hourly);
    assert!(back.errors.is_empty(), "{:?}", back.errors);
    assert_eq!(back.events_new, 0);
    assert_eq!(back.messages_new, 0);
    assert_eq!(back.sources_scanned, SOURCES);
    assert_eq!(
        w.env
            .count("SELECT count(*) FROM source_file WHERE missing = 1"),
        0
    );
    assert_eq!(
        w.env.count("SELECT count(*) FROM source_file"),
        SOURCES as i64
    );
    assert_eq!(totals(&w.db()), before);
    w.assert_consistent();
    assert_eq!(run_as(&collector, RunTrigger::Hourly).events_new, 0);
    assert_eq!(totals(&w.db()), before);
}

fn integer_cost(t: [i64; 5]) -> i64 {
    let [input, output, cw5m, cw1h, read] = t;
    input * 2 + output * 10 + cw5m * 3 + cw1h * 4 + read
}

#[test]
fn back_dated_price_override_reprices_sessions_rollups_and_kpi() {
    let w = World::new();
    let collector = w.builtin();
    run_as(&collector, RunTrigger::Startup);
    let cut: Timestamp = "2026-11-21T00:00:00Z".parse().unwrap();

    type Row = (String, [i64; 5], i64, Option<i64>, Option<i64>);
    let snapshot = || -> BTreeMap<i64, Row> {
        let db = w.db();
        let mut stmt = db
            .prepare(
                "SELECT id, ts, input_tokens, output_tokens, cache_write_5m_tokens,
                        cache_write_1h_tokens, cache_read_tokens, cost_micros, price_id,
                        reported_cost_micros
                 FROM usage_event WHERE provider = 'anthropic' AND model = ?1",
            )
            .unwrap();
        stmt.query_map([OPUS], |r| {
            Ok((
                r.get(0)?,
                (
                    r.get(1)?,
                    [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let reported = |w: &World| -> Vec<(i64, Option<i64>)> {
        let db = w.db();
        let mut stmt = db
            .prepare("SELECT id, reported_cost_micros FROM usage_event ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let others = |w: &World| -> Vec<(i64, i64, Option<i64>)> {
        let db = w.db();
        let mut stmt = db
            .prepare(
                "SELECT id, cost_micros, price_id FROM usage_event
                 WHERE NOT (provider = 'anthropic' AND model = ?1) ORDER BY id",
            )
            .unwrap();
        stmt.query_map([OPUS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };

    let before = snapshot();
    let reported_before = reported(&w);
    let others_before = others(&w);
    let kpi_before = w.env.store().kpi_summary(&all_time()).unwrap();
    assert!(before.len() > 10);
    assert!(before.values().any(|r| r.0.as_str() < "2026-11-21"));
    assert!(before.values().any(|r| r.0.as_str() >= "2026-11-21"));
    assert!(before.values().all(|r| r.3.is_some()));
    let harnesses: i64 = w.env.count(&format!(
        "SELECT count(DISTINCT harness_id) FROM usage_event WHERE model = '{OPUS}'"
    ));
    assert!(harnesses >= 2, "the override spans harnesses");

    let id = {
        let mut store = Store::open(&tbt_store::paths::db_path(&w.env.data)).unwrap();
        let id = store
            .insert_price(&ModelPrice {
                id: None,
                provider: "anthropic".into(),
                model_pattern: OPUS.into(),
                effective_from: cut,
                effective_to: None,
                input_per_mtok: "2".parse().unwrap(),
                output_per_mtok: "10".parse().unwrap(),
                cache_write_5m_per_mtok: Some("3".parse().unwrap()),
                cache_write_1h_per_mtok: Some("4".parse().unwrap()),
                cache_read_per_mtok: Some("1".parse().unwrap()),
                long_context: None,
                source: PriceSource::User,
            })
            .unwrap();
        let report = recompute_for_price(&mut store, id).unwrap();
        assert!(report.events_changed > 0);
        assert!(report.sessions_rebuilt > 0 && report.hours_rebuilt > 0);
        id
    };

    let check = |w: &World| {
        let after = snapshot();
        assert_eq!(after.len(), before.len());
        let mut changed = 0;
        for (event, (ts, tokens, cost, price, reported)) in &after {
            let old = &before[event];
            assert_eq!(reported, &old.4);
            if ts.as_str() >= "2026-11-21" {
                assert_eq!(*cost, integer_cost(*tokens), "event {event} at {ts}");
                assert_eq!(*price, Some(id.0), "event {event}");
                changed += 1;
            } else {
                assert_eq!((cost, price), (&old.2, &old.3), "event {event} at {ts}");
            }
        }
        assert!(changed > 0);
        assert_eq!(reported(w), reported_before);
        assert_eq!(others(w), others_before);
        w.assert_consistent();

        let kpi = w.env.store().kpi_summary(&all_time()).unwrap();
        let shift: i64 = after
            .iter()
            .map(|(event, row)| row.2 - before[event].2)
            .sum();
        assert_ne!(shift, 0);
        assert_eq!(kpi.cost, Micros(kpi_before.cost.0 + shift));
        assert_eq!(kpi.events, kpi_before.events);
        let session_cost: i64 = w.env.count("SELECT sum(cost_micros) FROM session");
        assert_eq!(Micros(session_cost), kpi.cost);
    };
    check(&w);

    let again = run_as(&collector, RunTrigger::Hourly);
    assert!(again.errors.is_empty(), "{:?}", again.errors);
    assert_eq!(again.events_new, 0);
    check(&w);
}

#[test]
fn wave2_database_continues_with_current_code_and_matches_a_fresh_backfill() {
    let fresh = World::new();
    run_as(&fresh.builtin(), RunTrigger::Startup);
    let want = totals(&fresh.db());

    let wave2 = fixtures_root().join("db/wave2/tbt.db");
    let w = World::with_db(Some(&wave2));
    let added_since = golden()["cursor"] + golden()["copilot_cli"] + golden()["grok_build"];
    let new_sources = 3 + 4;
    assert_eq!(
        w.env.count("SELECT count(*) FROM usage_event"),
        GOLDEN_TOTAL as i64 - added_since
    );
    let collector = w.builtin();
    let report = run_as(&collector, RunTrigger::Hourly);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.events_new, added_since as u64);
    assert_eq!(report.sources_scanned, SOURCES);
    assert_eq!(report.sources_missing, SOURCES - new_sources);
    assert_eq!(totals(&w.db()), want);
    w.assert_consistent();
    assert_quiet_rerun(&run_as(&collector, RunTrigger::Hourly));
    assert_eq!(w.per_harness(), golden());
}

#[test]
fn resume_after_the_original_file_was_cleaned_up_counts_only_new_calls() {
    let w = World::new();
    let collector = w.builtin();
    run_as(&collector, RunTrigger::Startup);
    let original = w.session_row("claude_code", LONG_SESSION);
    let before = w.per_harness();

    claude_resume(
        &w,
        LONG_SESSION,
        CLAUDE_RESUME_ID,
        &["2026-11-24T05:10:00.000Z", "2026-11-24T05:20:00.000Z"],
    );
    std::fs::remove_file(w.env.find(&format!("{LONG_SESSION}.jsonl"))).unwrap();

    let report = run_as(&collector, RunTrigger::Hourly);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.sources_missing, 1);
    assert_eq!(report.events_new, 2);
    assert_eq!(
        delta(&w.per_harness(), &before),
        expected(&[("claude_code", 2)])
    );
    assert_eq!(w.session_row("claude_code", LONG_SESSION), original);
    assert_eq!(
        w.env
            .session_link(CLAUDE_RESUME_ID, "resumed_from_session_id")
            .as_deref(),
        Some(LONG_SESSION)
    );
    assert_eq!(w.session_row("claude_code", CLAUDE_RESUME_ID).sums[0], 2);
    w.assert_consistent();
    assert_eq!(run_as(&collector, RunTrigger::Hourly).events_new, 0);
}

fn split_append(path: &Path, line: &str, at: usize) -> String {
    append(path, &line[..at]);
    line[at..].to_owned()
}

#[test]
fn lines_half_written_during_a_run_are_counted_once_complete() {
    let w = World::new();
    let collector = w.builtin();
    run_as(&collector, RunTrigger::Startup);
    let before = w.per_harness();

    let claude = w.env.find(&format!("{SHORT_SESSION}.jsonl"));
    let template = last_claude_call(&std::fs::read_to_string(&claude).unwrap());
    let call = claude_call(
        &template,
        SHORT_SESSION,
        100,
        "2026-11-24T10:00:00.000Z",
        &template["uuid"],
    )
    .to_string()
        + "\n";
    let claude_rest = split_append(&claude, &call, call.len() / 2);

    let codex = w.env.find(CODEX_CONTINUATION);
    let turn = codex_turn(
        258,
        "2026-11-24T10:00:00.000Z",
        "resp_e2e_partial_1",
        [10_000, 9_000, 50, 0],
        843_714,
    );
    let record = turn.find("token_usage_record").unwrap();
    let codex_rest = split_append(&codex, &turn, record);

    let proxy = w.env.find(PROXY_DAY);
    let request = json!({
        "id": "chatcmpl-e2e-partial", "created_ms": 1_795_428_000_000_i64, "session": "e2e",
        "provider": "openai", "model": "gpt-5.1-mini",
        "usage": {"prompt_tokens": 400, "completion_tokens": 20}
    })
    .to_string()
        + "\n";
    let proxy_rest = split_append(&proxy, &request, 10);

    let partial = run_as(&collector, RunTrigger::Hourly);
    assert!(partial.errors.is_empty(), "{:?}", partial.errors);
    assert!(partial.warnings.is_empty(), "{:?}", partial.warnings);
    assert_eq!(partial.events_new, 0);

    append(&claude, &claude_rest);
    append(&codex, &codex_rest);
    append(&proxy, &proxy_rest);
    let complete = run_as(&collector, RunTrigger::Hourly);
    assert!(complete.errors.is_empty(), "{:?}", complete.errors);
    assert!(complete.warnings.is_empty(), "{:?}", complete.warnings);
    assert_eq!(complete.events_new, 3);
    assert_eq!(
        delta(&w.per_harness(), &before),
        expected(&[("claude_code", 1), ("codex", 1), (LLM_PROXY, 1)])
    );
    w.assert_consistent();
    assert_eq!(run_as(&collector, RunTrigger::Hourly).events_new, 0);
}

#[test]
fn legacy_gemini_session_rewritten_whole_adds_only_the_new_message() {
    let w = World::new();
    let collector = w.clocked();
    run_as(&collector, RunTrigger::Startup);
    let before = w.per_harness();
    let totals_before = totals(&w.db());

    let path = w
        .root("gemini_cli/0.71.0-nightly")
        .join("tmp/bold-birch-00/chats")
        .join(GEMINI_RESUMED.replace(".jsonl", ".json"));
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let messages = doc["messages"].as_array_mut().unwrap();
    messages.push(json!({
        "id": "e2e0e2e0-0000-4000-8000-0000000b0001", "timestamp": "2026-11-24T11:00:00.000Z",
        "type": "user", "content": [{"text": "lorem"}]
    }));
    messages.push(json!({
        "id": "e2e0e2e0-0000-4000-8000-0000000b0002", "timestamp": "2026-11-24T11:00:04.000Z",
        "type": "gemini", "content": "ipsum", "thoughts": [], "model": "gemini-2.5-flash",
        "tokens": {"input": 3_000, "output": 40, "cached": 2_000, "thoughts": 0, "tool": 0,
                   "total": 3_040}
    }));
    doc["lastUpdated"] = json!("2026-11-24T11:00:04.100Z");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    w.set_clock(mtime(&path) + jiff::SignedDuration::from_secs(30));
    let held = run_as(&collector, RunTrigger::Hourly);
    assert!(held.errors.is_empty(), "{:?}", held.errors);
    assert_eq!(held.events_new, 0);
    let held_totals = totals(&w.db());
    assert_eq!(held_totals.event_sums, totals_before.event_sums);
    assert_eq!(held_totals.hourly, totals_before.hourly);

    w.set_clock(mtime(&path) + jiff::SignedDuration::from_mins(10));
    let released = run_as(&collector, RunTrigger::Hourly);
    assert!(released.errors.is_empty(), "{:?}", released.errors);
    assert_eq!(released.events_new, 1);
    assert_eq!(
        delta(&w.per_harness(), &before),
        expected(&[("gemini_cli", 1)])
    );
    w.assert_consistent();
    for _ in 0..2 {
        assert_eq!(run_as(&collector, RunTrigger::Hourly).events_new, 0);
    }
}
