#!/usr/bin/env python3

import json
import shutil
from compression import zstd
from pathlib import Path

ROOT = Path(__file__).resolve().parent / "v4" / "sessions"
LOREM = "/home/user/projects/lorem"
IPSUM = "/home/user/projects/ipsum"
T0 = 1795165200000
FLASH = {"provider": "deepseek-official", "model": "deepseek-v4-flash"}
PRO = {"provider": "deepseek-official", "model": "deepseek-v4-pro"}


def project_dir(cwd):
    return "_no-cwd" if cwd is None else "--" + cwd.strip("/").replace("/", "-") + "--"


def header(sid, cwd, version=4, **extra):
    h = {"type": "session", "version": version, "id": sid, "createdAt": T0}
    if cwd is not None:
        h["cwd"] = cwd
    h.update({"isSeeded": False, "delegationDepth": 0})
    h.update(extra)
    return h


class Log:
    def __init__(self, time=T0):
        self.rows = []
        self.time = time

    def add(self, kind, data, step=1000, **extra):
        self.time += step
        row = {"type": kind, "seq": len(self.rows), "time": self.time, "data": data}
        row.update(extra)
        self.rows.append(row)
        return self

    def user(self, text):
        return self.add(
            "user/message",
            {"role": "user", "id": f"msg-{len(self.rows)}", "content": [{"type": "text", "text": text}],
             "source": {"kind": "human"}},
            surfaceOp="append",
        )

    def route(self, route):
        self.add("request/header", {"header": {"config": dict(route), "tools": []}, "reason": "initial"})
        return self.add("request/context", dict(route, contextWindow=128000))

    def assistant(self, turn, step, route, usage, text="Done."):
        return self.add(
            "assistant/message",
            {
                "turn": turn,
                "step": step,
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}],
                    "source": dict(route, kind="model"),
                    "id": f"msg-{len(self.rows)}",
                },
                "stream": [{"type": "chunk", "time": self.time, "chunk": {"type": "usage", "usage": usage}}],
                "usage": usage,
            },
            step=4000,
            surfaceOp="append",
        )


def lines(rows):
    return "".join(json.dumps(r, separators=(",", ":")) + "\n" for r in rows).encode()


def frame(rows):
    return zstd.compress(lines(rows), options={zstd.CompressionParameter.checksum_flag: 1})


def write(path, head, rows, batches=None, tail=b""):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.suffix != ".zstd":
        path.write_bytes(lines([head] + rows) + tail)
        return
    out = frame([head])
    start = 0
    for end in (batches or []) + [len(rows)]:
        if end > start:
            out += frame(rows[start:end])
        start = end
    path.write_bytes(out + tail)


def main_session():
    log = Log()
    log.add("turn/start", {"turn": 1}).add("step/start", {"turn": 1, "step": 1})
    log.user("The lorem test fails one run in ten. Find out why.")
    log.route(FLASH)
    log.add("session/title", {"title": "Fix the flaky lorem test", "messageSeqs": [2], "source": {"kind": "fallback"}})
    log.assistant(1, 1, FLASH, {"inputTokens": 8082, "outputTokens": 235, "cacheReadTokens": 384, "reasoningTokens": 75})
    log.add("tool/call", {"turn": 1, "step": 1, "callId": "call_1", "name": "read", "arguments": "{\"path\":\"tests/lorem.rs\"}"})
    log.add("tool/result", {"turn": 1, "step": 1, "message": {"role": "tool", "id": "msg-tr1", "toolCallId": "call_1",
                                                               "source": {"kind": "tool", "callId": "call_1"},
                                                               "content": [{"type": "text", "text": "fn lorem() {}"}]}},
            surfaceOp="append")
    log.add("step/end", {"turn": 1, "step": 1}).add("step/start", {"turn": 1, "step": 2})
    log.add("assistant/attempt", {"turn": 1, "step": 2, "stream": [
        {"type": "chunk", "time": log.time, "chunk": {"type": "usage", "usage": {"inputTokens": 150, "outputTokens": 12, "cacheReadTokens": 8320}}},
        {"type": "chunk", "time": log.time, "chunk": {"type": "finish", "reason": {"kind": "error", "failure": {"message": "stream reset", "code": "STREAM_ERROR"}}}},
    ]})
    log.assistant(1, 2, FLASH, {"inputTokens": 199, "outputTokens": 84, "totalTokens": 9499, "cacheReadTokens": 9216, "reasoningTokens": 81},
                  "The test races the temp-dir cleanup.")
    log.add("step/end", {"turn": 1, "step": 2}).add("turn/end", {"turn": 1, "reason": {"kind": "completed"}})
    first_turn = len(log.rows)

    log.add("turn/start", {"turn": 2}, step=60000).add("step/start", {"turn": 2, "step": 1})
    log.user("Fix it with the pro model.")
    log.route(PRO)
    log.assistant(2, 1, PRO, {"inputTokens": 1316, "outputTokens": 249, "totalTokens": 9117, "cacheReadTokens": 7552, "reasoningTokens": 72})
    log.add("compaction/start", {"compactionId": "cmp-1"})
    log.add("compaction/summary", dict(FLASH, compactionId="cmp-1", summary=[{"type": "text", "text": "Flaky lorem test traced to temp-dir cleanup."}],
                                       llmStreamCall=True, maxTokens=1024, usage={"inputTokens": 9000, "outputTokens": 300}))
    log.add("compaction/end", {"compactionId": "cmp-1"})
    log.add("step/end", {"turn": 2, "step": 1}).add("turn/end", {"turn": 2, "reason": {"kind": "completed"}})

    torn = Log(log.time)
    torn.rows = []
    torn.add("turn/start", {"turn": 3}, step=60000)
    torn.rows[0]["seq"] = len(log.rows)
    torn.assistant(3, 1, PRO, {"inputTokens": 5000, "outputTokens": 500})
    torn.rows[1]["seq"] = len(log.rows) + 1
    torn_frame = frame(torn.rows)
    return log, first_turn, torn_frame[: len(torn_frame) // 2]


def build():
    if ROOT.exists():
        shutil.rmtree(ROOT)
    lorem = ROOT / project_dir(LOREM)

    main, first_turn, torn = main_session()
    write(lorem / "ds-main" / "session.v4.jsonl.zstd", header("ds-main", LOREM, agentPreset="standard"),
          main.rows, batches=[first_turn], tail=torn)

    sub = Log(T0 + 20000)
    sub.add("turn/start", {"turn": 1}).add("step/start", {"turn": 1, "step": 1})
    sub.user("List the files that create temp dirs.")
    sub.route({"provider": "deepseek-account", "model": "deepseek-v4-flash"})
    sub.assistant(1, 1, {"provider": "deepseek-account", "model": "deepseek-v4-flash"},
                  {"inputTokens": 97, "outputTokens": 26, "cacheReadTokens": 7808, "reasoningTokens": 23})
    sub.add("step/end", {"turn": 1, "step": 1}).add("turn/end", {"turn": 1, "reason": {"kind": "completed"}})
    write(lorem / "ds-sub" / "session.v4.jsonl.zstd",
          header("ds-sub", LOREM, parentSession="ds-main", origin="subagent", delegationDepth=1), sub.rows)

    fork = Log(main.time)
    fork.rows = [dict(r) for r in main.rows[:first_turn]]
    fork.time = T0 + 3_600_000
    fork.add("session/end-seed", {"inherited": True})
    fork.add("turn/start", {"turn": 2}).add("step/start", {"turn": 2, "step": 1})
    fork.user("Try a different approach instead.")
    kimi = {"provider": "moonshotai", "model": "kimi-k2.6"}
    fork.route(kimi)
    fork.assistant(2, 1, kimi, {"inputTokens": 500, "outputTokens": 40})
    fork.add("step/end", {"turn": 2, "step": 1}).add("turn/end", {"turn": 2, "reason": {"kind": "completed"}})
    fork_header = header("ds-fork", LOREM, parentSession="ds-main")
    fork_header["isSeeded"] = True
    fork_header["createdAt"] = T0 + 3_600_000
    write(lorem / "ds-fork" / "session.v4.jsonl.zstd", fork_header, fork.rows)

    old = Log(T0 - 86_400_000 * 30)
    old.add("turn/start", {"turn": 1}).add("step/start", {"turn": 1, "step": 1})
    old.user("Summarize the ipsum module.")
    old.route(FLASH)
    old.assistant(1, 1, FLASH, {"inputTokens": 981, "outputTokens": 695, "totalTokens": 9356, "cacheReadTokens": 7680, "reasoningTokens": 329})
    old.add("step/end", {"turn": 1, "step": 1}).add("turn/end", {"turn": 1, "reason": {"kind": "completed"}})
    migrated = ROOT / project_dir(IPSUM) / "ds-migrated"
    old_header = header("ds-migrated", IPSUM, version=3)
    old_header["createdAt"] = old.rows[0]["time"] - 1000
    write(migrated / "session.v3.jsonl.zstd", old_header, old.rows)
    write(migrated / "session.v4.jsonl.zstd", dict(old_header, version=4), old.rows)

    plain = Log(T0 + 7_200_000)
    plain.add("turn/start", {"turn": 1}).add("step/start", {"turn": 1, "step": 1})
    plain.user("Draft a commit message.")
    sonnet = {"provider": "anthropic", "model": "claude-sonnet-5-5"}
    plain.route(sonnet)
    plain.assistant(1, 1, sonnet, {"inputTokens": 12, "outputTokens": 300, "cacheReadTokens": 0, "cacheWriteTokens": 4000})
    plain.add("step/end", {"turn": 1, "step": 1})
    plain_header = header("ds-plain", None)
    plain_header["createdAt"] = T0 + 7_200_000
    write(ROOT / "_no-cwd" / "ds-plain" / "session.v4.jsonl", plain_header, plain.rows,
          tail=b'{"type":"turn/end","seq":99,"time":')


if __name__ == "__main__":
    build()
