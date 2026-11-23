#!/usr/bin/env python3
"""Anonymize harness logs (JSONL / JSON / SQLite) for use as test fixtures.

Usage:
  scripts/anonymize.py SRC DST [--salt S] [--text-cap N] [--max-bytes N]
                       [--user NAME]... [--host NAME]... [--scrub WORD]...

SRC may be a file or a directory; DST is the matching output file or directory
(a directory DST is recreated from scratch). Output is deterministic: the same
input and salt always give byte-identical output, and the same real value
always maps to the same fake value, across files and across the DB.
"""

import argparse
import getpass
import hashlib
import hmac
import json
import os
import re
import shutil
import socket
import sqlite3
import sys

DEFAULT_SALT = "tokenburntrack-fixtures-v1"

UUID_RE = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
EMAIL_RE = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
ISO_RE = re.compile(r"^\d{4}-\d{2}-\d{2}([T ][\d:.]+(Z|[+-]\d{2}:?\d{2})?)?$")
NUM_RE = re.compile(r"^-?\d+(\.\d+)?$")
OPENCODE_ID_RE = re.compile(r"^([a-z]{3}_)([0-9a-f]{12})([0-9A-Za-z]{14})$")
PREFIXED_RE = re.compile(r"^([A-Za-z]{2,12}[_-])([A-Za-z0-9_-]{8,})$")
AGENT_ID_RE = re.compile(r"^a[0-9a-f]{16}$")
HEX_RE = re.compile(r"^[0-9a-f]{16,}$")
URL_RE = re.compile(r"^[a-z][a-z0-9+.-]*://\S+$")
ID_KEY_RE = re.compile(
    r"(^id$|^ids$|_id$|_ids$|Id$|Ids$|ID$|IDs$|uuid$|Uuid$|UUID$|uuids$|Uuids$"
    r"|hash$|Hash$|digest$|Digest$|^requestRef$|^leafUuid$|^\$rewindTo$|^removeIds$|^orderIds$)"
)

KEEP_KEYS = {
    "type", "subtype", "role", "model", "requestedModel", "advisorModel", "modelID", "modelId",
    "providerID", "provider", "model_provider", "model_provider_id", "version", "cli_version",
    "timestamp", "startTime", "lastUpdated", "created_at", "updated_at", "current_date",
    "stop_reason", "finish", "finish_reason", "service_tier", "speed", "inference_geo",
    "operation", "mode", "permissionMode", "userType", "entrypoint", "level", "effort",
    "perTurnEffort", "reasoning_effort", "querySource", "originator", "source", "status", "phase",
    "variant", "agent", "thinkingDisplay", "sessionKind", "promptSource", "turnOrigin",
    "turnPosition", "renderedRole", "madeBy", "approval_policy", "approvals_reviewer",
    "collaboration_mode_kind", "personality", "kind", "history_mode", "thread_source",
    "multi_agent_version", "multi_agent_mode", "tool", "tool_name", "namespace", "plan_type",
    "limit_id", "codex_error_info", "trigger", "via", "priority", "vcs", "icon_color", "strategy",
    "betas", "shell", "platform", "media_type", "promptCacheRequested",
}
MODEL_REF_KEYS = {"id", "providerID", "variant", "name"}
ENUMISH_KEYS = {"summary", "reason", "access"}
ENUM_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_.:-]{0,40}$")
TOOL_PARENT_TYPES = {
    "tool_use", "server_tool_use", "mcp_tool_use", "function_call", "custom_tool_call",
    "local_shell_call", "tool", "functionCall",
}
NAME_LIST_KEYS = {"tools", "toolSequence", "addedNames", "removedNames", "readdedNames", "surfacedNames"}
PATH_KEYS = {
    "cwd", "path", "filePath", "file_path", "filepath", "notebook_path", "workingDirectory",
    "directory", "directories", "worktree", "root", "roots", "workspace_roots",
    "runtime_workspace_roots", "additionalWorkingDirectories", "scratchpadDirectory",
    "trackingPath", "realParentDir", "displayPath", "file", "files", "filename", "transcript_path",
    "project_path", "projectPath", "dir", "sandboxes", "touchedPaths",
}
PATH_KEY_PARENTS = {"trackedFileBackups"}
BRANCH_KEYS = {"gitBranch", "branch", "git_branch"}
BRANCH_KEEP = {"", "main", "master", "HEAD", "develop", "dev", "trunk"}
BLOB_KEYS = {"signature", "encrypted_content", "redacted_thinking"}
TIMEZONE_KEYS = {"timezone", "timeZone"}

SAFE_SEGMENTS = {
    "", ".", "..", "~", "home", "Users", "tmp", "var", "mnt", "opt", "usr", "etc", "bin", "lib",
    "share", "src", "tests", "test", "projects", "project", "subagents", "scratchpad", "memory",
    "sessions", "chats", "docs", "scripts", "crates", "target", "build", "dist", "node_modules",
    "worktrees", ".claude", ".codex", ".gemini", ".local", ".config", ".git", "opencode", "storage",
    "packages", "app", "assets", "include", "pages", "components", "public", "fixtures",
    "README.md", "Cargo.toml", "Cargo.lock", "package.json", ".gitignore", "CLAUDE.md", "AGENTS.md",
}

WORDS = (
    "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut "
    "labore et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris "
    "nisi aliquip ex ea commodo consequat duis aute irure in reprehenderit voluptate velit esse "
    "cillum fugiat nulla pariatur excepteur sint occaecat cupidatat non proident sunt culpa qui "
    "officia deserunt mollit anim id est laborum"
).split()
ADJ = "amber bold calm dusky eager fern gilded hazy ivory jolly keen lunar misty noble olive pale".split()
NOUN = "acorn birch cedar delta ember fjord grove harbor islet juniper kestrel lagoon meadow orchid pine quartz".split()

ESSENTIAL_TYPES = {
    "session_meta", "turn_context", "summary", "custom-title", "ai-title", "continued-in",
    "derived-from", "cost-state", "compacted", "token_usage_record", "agent-name",
}
ESSENTIAL_PAYLOAD_TYPES = {
    "token_count", "task_started", "task_complete", "turn_aborted", "thread_rolled_back",
    "context_compacted", "user_message",
}
TAIL_KEEP_TYPES = {"session_meta", "continued-in", "derived-from", "custom-title", "ai-title", "summary", "cost-state"}
LINK_KEYS = ("parentUuid", "logicalParentUuid", "leafUuid", "sourceToolAssistantUUID")


class Anonymizer:
    def __init__(self, salt, text_cap, users, hosts, scrub_words):
        self.key = salt.encode()
        self.text_cap = text_cap
        self.users = {u for u in users if u}
        self.hosts = {h for h in hosts if h}
        self.scrub_words = sorted({w for w in scrub_words if w}, key=len, reverse=True)
        self.seen_cwds = set()
        self._cache = {}

    def _digest(self, kind, value, n=32):
        out = b""
        counter = 0
        while len(out) < n:
            msg = b"%s\0%d\0%s" % (kind.encode(), counter, value.encode("utf-8", "surrogatepass"))
            out += hmac.new(self.key, msg, hashlib.sha256).digest()
            counter += 1
        return out[:n]

    def _hex(self, kind, value, n):
        return self._digest(kind, value, (n + 1) // 2).hex()[:n]

    def _shape(self, kind, value):
        if all(c in "0123456789abcdef" for c in value):
            return self._hex(kind, value, len(value))
        d = self._digest(kind, value, len(value))
        out = []
        for c, b in zip(value, d):
            if c.isdigit():
                out.append("0123456789"[b % 10])
            elif "a" <= c <= "z":
                out.append(chr(ord("a") + b % 26))
            elif "A" <= c <= "Z":
                out.append(chr(ord("A") + b % 26))
            else:
                out.append(c)
        return "".join(out)

    def map_uuid(self, u):
        low = u.lower()
        h = self._hex("uuid", low, 32)
        ver = low[14]
        var = low[19]
        if ver == "7":
            res = low[:13] + "-7" + h[0:3] + "-" + var + h[3:6] + "-" + h[6:18]
        else:
            res = h[0:8] + "-" + h[8:12] + "-" + ver + h[12:15] + "-" + var + h[15:18] + "-" + h[18:30]
        return res.upper() if u.isupper() else res

    def map_id(self, value):
        if value in self._cache:
            return self._cache[value]
        if UUID_RE.search(value):
            res = UUID_RE.sub(lambda m: self.map_uuid(m.group(0)), value)
        elif AGENT_ID_RE.match(value):
            res = "a" + self._hex("agent", value, 16)
        else:
            m = OPENCODE_ID_RE.match(value)
            if m:
                res = m.group(1) + m.group(2) + self._shape("id", m.group(3))
            else:
                m = PREFIXED_RE.match(value)
                if m and any(c.isdigit() for c in m.group(2)):
                    res = m.group(1) + self._shape("id", m.group(2))
                else:
                    res = self._shape("id", value)
        self._cache[value] = res
        return res

    def looks_like_id(self, value):
        if UUID_RE.fullmatch(value) or AGENT_ID_RE.match(value) or HEX_RE.match(value):
            return True
        if OPENCODE_ID_RE.match(value):
            return True
        m = PREFIXED_RE.match(value)
        return bool(m and sum(c.isdigit() for c in m.group(2)) >= 2 and len(m.group(2)) >= 12)

    def fake_name(self, value):
        d = self._digest("name", value, 3)
        return "%s-%s-%02x" % (ADJ[d[0] % len(ADJ)], NOUN[d[1] % len(NOUN)], d[2])

    def map_segment(self, seg, prev):
        if seg == "home" and prev == "":
            return "u"
        if seg in SAFE_SEGMENTS:
            return seg
        if seg in self.users or (prev in ("home", "Users") and not UUID_RE.search(seg)):
            return "user"
        if UUID_RE.search(seg):
            return UUID_RE.sub(lambda m: self.map_uuid(m.group(0)), seg)
        m = re.match(r"^(agent-)(a[0-9a-f]{16})(\.\w+)?$", seg)
        if m:
            return m.group(1) + self.map_id(m.group(2)) + (m.group(3) or "")
        slug = self._slug_for(seg)
        if slug is not None:
            return slug
        base, dot, ext = seg.rpartition(".")
        if dot and base and re.fullmatch(r"[A-Za-z0-9]{1,6}", ext):
            return self.fake_name(base) + "." + ext
        return self.fake_name(seg)

    def map_file_segment(self, seg):
        if UUID_RE.search(seg):
            return UUID_RE.sub(lambda m: self.map_uuid(m.group(0)), seg)
        m = re.match(r"^(agent-)(a[0-9a-f]{16})(\.\w+)?$", seg)
        if m:
            return m.group(1) + self.map_id(m.group(2)) + (m.group(3) or "")
        slug = self._slug_for(seg)
        return slug if slug is not None else self.scrub(seg)

    def _slug_for(self, seg):
        if not seg.startswith("-"):
            return None
        for cwd in self.seen_cwds:
            if claude_slug(cwd) == seg:
                return claude_slug(self.map_path(cwd))
        return None

    def map_path(self, value):
        if "\n" in value or len(value) > 1024:
            return self.lorem(value)
        if value.startswith("file://"):
            return "file://" + self.map_path(value[7:])
        parts = value.split("/")
        out = []
        prev = None
        for seg in parts:
            out.append(self.map_segment(seg, prev))
            prev = seg
        return "/".join(out)

    def map_branch(self, value):
        if value in BRANCH_KEEP:
            return value
        return "branch-" + self._hex("branch", value, 6)

    def map_url(self, value):
        return "https://example.invalid/" + self._hex("url", value, 10)

    def map_blob(self, value):
        return self._shape("blob", value[:64])

    def lorem(self, value):
        n = min(len(value), self.text_cap)
        if n == 0:
            return value
        d = self._digest("text", value, 64)
        words = []
        length = 0
        i = 0
        while length < n:
            w = WORDS[(d[i % len(d)] + 7 * (i // len(d))) % len(WORDS)]
            words.append(w)
            length += len(w) + 1
            i += 1
        return " ".join(words)[:n].rstrip() or "lorem"

    def scrub(self, value):
        if not value:
            return value
        if "@" in value:
            value = EMAIL_RE.sub("user@example.invalid", value)
        low = value.lower()
        for u in self.users:
            if u.lower() in low:
                value = re.sub(re.escape(u), "user", value, flags=re.I)
                low = value.lower()
        for h in self.hosts:
            if h.lower() in low:
                value = re.sub(re.escape(h), "host", value, flags=re.I)
                low = value.lower()
        for w in self.scrub_words:
            if w.lower() in low:
                value = re.sub(re.escape(w), self.fake_name(w.lower()), value, flags=re.I)
                low = value.lower()
        return value

    def learn(self, obj):
        if isinstance(obj, dict):
            for k, v in obj.items():
                if k in ("cwd", "directory", "worktree", "workingDirectory") and isinstance(v, str) and v.startswith("/"):
                    self.seen_cwds.add(v)
                else:
                    self.learn(v)
        elif isinstance(obj, list):
            for v in obj:
                self.learn(v)

    def string(self, key, value, parent):
        if key in KEEP_KEYS or (key == "id" and isinstance(parent, dict) and "providerID" in parent and set(parent) <= MODEL_REF_KEYS):
            if "\n" in value or len(value) > 120:
                return self.lorem(value)
            if value.startswith("/"):
                return self.map_path(value)
            return self.scrub(value)
        if key in ENUMISH_KEYS and ENUM_RE.match(value):
            return self.scrub(value)
        if key == "name" and isinstance(parent, dict) and (
            parent.get("type") in TOOL_PARENT_TYPES or any(k in parent for k in ("args", "input", "arguments", "call_id", "callID"))
        ) and ENUM_RE.match(value):
            return self.scrub(value)
        if key in NAME_LIST_KEYS and ENUM_RE.match(value):
            return self.scrub(value)
        if key in BLOB_KEYS:
            return self.map_blob(value)
        if key in TIMEZONE_KEYS:
            return "UTC"
        if key in BRANCH_KEYS:
            return self.map_branch(value)
        if key in PATH_KEYS:
            if URL_RE.match(value):
                return self.map_url(value)
            return self.map_path(value)
        if ID_KEY_RE.search(key) and value and not any(c.isspace() for c in value):
            if value.startswith("/"):
                return self.map_path(value)
            return self.map_id(value)
        if value and self.looks_like_id(value):
            return self.map_id(value)
        if ISO_RE.match(value) or NUM_RE.match(value) or value in ("true", "false", "null"):
            return value
        if URL_RE.match(value):
            return self.map_url(value)
        if value.startswith("/") and len(value) < 512 and not any(c.isspace() for c in value):
            return self.map_path(value)
        if value[:1] in "{[":
            try:
                inner = json.loads(value)
            except ValueError:
                inner = None
            if isinstance(inner, (dict, list)):
                compact = ", " not in value and ": " not in value
                return dumps(self.value(key, inner, parent), compact)
        return self.lorem(value)

    def dict_key(self, key, parent_key):
        if UUID_RE.search(key):
            return UUID_RE.sub(lambda m: self.map_uuid(m.group(0)), key)
        if self.looks_like_id(key):
            return self.map_id(key)
        if parent_key in PATH_KEY_PARENTS or "/" in key or "\\" in key:
            return self.map_path(key)
        return self.scrub(key)

    def value(self, key, v, parent=None):
        if isinstance(v, dict):
            return {self.dict_key(k, key): self.value(k, x, v) for k, x in v.items()}
        if isinstance(v, list):
            return [self.value(key, x, parent) for x in v]
        if isinstance(v, str):
            return self.string(key, v, parent)
        return v


def dumps(obj, compact=True):
    if compact:
        return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    return json.dumps(obj, ensure_ascii=False)


def claude_slug(path):
    return re.sub(r"[^A-Za-z0-9]", "-", path)


def is_essential(obj, index):
    if index < 3 or not isinstance(obj, dict):
        return True
    t = obj.get("type")
    if t in ESSENTIAL_TYPES:
        return True
    p = obj.get("payload")
    if isinstance(p, dict) and p.get("type") in ESSENTIAL_PAYLOAD_TYPES:
        return True
    m = obj.get("message")
    if isinstance(m, dict) and m.get("usage"):
        return True
    if "tokens" in obj or "$rewindTo" in obj or "$set" in obj or "projectHash" in obj:
        return True
    return False


def trim(records, max_bytes):
    total = sum(len(t) + 1 for _, t in records)
    if not max_bytes or total <= max_bytes:
        return records
    order = sorted(
        (i for i, (o, _) in enumerate(records) if not is_essential(o, i)),
        key=lambda i: -len(records[i][1]),
    )
    dropped = set()
    for i in order:
        if total <= max_bytes:
            break
        dropped.add(i)
        total -= len(records[i][1]) + 1
    for i in range(len(records) - 1, 0, -1):
        if total <= max_bytes:
            break
        o = records[i][0]
        if i not in dropped and not (isinstance(o, dict) and o.get("type") in TAIL_KEEP_TYPES):
            dropped.add(i)
            total -= len(records[i][1]) + 1
    relink = {}
    for i in dropped:
        o = records[i][0]
        if isinstance(o, dict) and o.get("uuid"):
            relink[o["uuid"]] = o.get("parentUuid")
    out = []
    for i, (o, t) in enumerate(records):
        if i in dropped:
            continue
        if isinstance(o, dict) and any(o.get(k) in relink for k in LINK_KEYS):
            for k in LINK_KEYS:
                target = o.get(k)
                seen = set()
                while target in relink and target not in seen:
                    seen.add(target)
                    target = relink[target]
                if k in o:
                    o[k] = target
            t = dumps(o, compact=t_is_compact(t))
        out.append((o, t))
    return out


def t_is_compact(text):
    return '", "' not in text and '": ' not in text


def anonymize_jsonl_text(anon, text, max_bytes):
    records = []
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            records.append((None, anon.lorem(line)))
            continue
        new = anon.value(None, obj)
        records.append((new, dumps(new, compact=t_is_compact(line))))
    records = trim(records, max_bytes)
    return "".join(t + "\n" for _, t in records)


def anonymize_json_text(anon, text):
    obj = json.loads(text)
    new = anon.value(None, obj)
    indent = None
    m = re.match(r"^[\[{]\n( +)", text)
    if m:
        indent = len(m.group(1))
    if indent:
        out = json.dumps(new, ensure_ascii=False, indent=indent)
    else:
        out = dumps(new, compact=t_is_compact(text))
    return out + ("\n" if text.endswith("\n") else "")


SKIP_TABLES = {"__drizzle_migrations", "migration", "data_migration", "sqlite_sequence"}


def anonymize_db(anon, src, dst):
    if os.path.exists(dst):
        os.remove(dst)
    s = sqlite3.connect("file:%s?mode=ro" % src, uri=True)
    d = sqlite3.connect(dst)
    s.backup(d)
    s.close()
    d.execute("PRAGMA foreign_keys=OFF")
    tables = [r[0] for r in d.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")]
    for t in tables:
        for (data,) in d.execute('SELECT data FROM "%s"' % t) if "data" in columns(d, t) else []:
            if isinstance(data, str) and data[:1] == "{":
                try:
                    anon.learn(json.loads(data))
                except ValueError:
                    pass
        for c in ("directory", "worktree"):
            if c in columns(d, t):
                for (v,) in d.execute('SELECT "%s" FROM "%s"' % (c, t)):
                    if isinstance(v, str) and v.startswith("/"):
                        anon.seen_cwds.add(v)
    for t in sorted(tables):
        if t in SKIP_TABLES:
            continue
        cols = columns(d, t)
        rows = d.execute('SELECT rowid, * FROM "%s"' % t).fetchall()
        for row in rows:
            rowid, vals = row[0], row[1:]
            new = []
            for c, v in zip(cols, vals):
                if isinstance(v, str):
                    new.append(anon.string(c, v, None))
                elif isinstance(v, bytes):
                    new.append(b"")
                else:
                    new.append(v)
            if list(new) != list(vals):
                sets = ", ".join('"%s"=?' % c for c in cols)
                d.execute('UPDATE "%s" SET %s WHERE rowid=?' % (t, sets), new + [rowid])
    d.commit()
    d.execute("PRAGMA journal_mode=DELETE")
    d.execute("PRAGMA page_size=1024")
    d.execute("VACUUM")
    d.close()


def columns(db, table):
    return [r[1] for r in db.execute('PRAGMA table_info("%s")' % table)]


def kind_of(path):
    low = path.lower()
    if low.endswith(".jsonl"):
        return "jsonl"
    if low.endswith(".json"):
        return "json"
    if low.endswith((".db", ".sqlite", ".sqlite3")):
        return "db"
    return None


def fix_codex_history_base(out_files):
    metas = {}
    lines_of = {}
    for path in out_files:
        if not path.endswith(".jsonl"):
            continue
        with open(path, "rb") as fh:
            raw = fh.read().split(b"\n")
        lines_of[path] = raw
        try:
            first = json.loads(raw[0])
        except ValueError:
            continue
        if first.get("type") == "session_meta":
            tid = first.get("payload", {}).get("id")
            if tid and os.path.basename(path).endswith(tid + ".jsonl"):
                metas[tid] = path
    for path, raw in lines_of.items():
        try:
            first = json.loads(raw[0])
        except ValueError:
            continue
        hb = first.get("payload", {}).get("history_base") if first.get("type") == "session_meta" else None
        if not isinstance(hb, dict) or hb.get("thread_id") not in metas or metas[hb["thread_id"]] == path:
            continue
        base = lines_of[metas[hb["thread_id"]]]
        offset = 0
        target = None
        for line in base:
            if not line:
                continue
            o = json.loads(line)
            if o.get("ordinal") is not None and o["ordinal"] >= hb.get("end_ordinal_exclusive", 0):
                target = offset
                break
            offset += len(line) + 1
        hb["end_byte_offset"] = target if target is not None else offset
        raw[0] = dumps(first, compact=t_is_compact(raw[0].decode())).encode()
        with open(path, "wb") as fh:
            fh.write(b"\n".join(raw))


def process_dir(anon, src, dst, max_bytes):
    if os.path.exists(dst):
        shutil.rmtree(dst)
    entries = []
    for root, dirs, files in os.walk(src):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(root, f)
            if kind_of(p):
                entries.append(p)
    texts = {}
    for p in entries:
        if kind_of(p) != "db":
            with open(p, encoding="utf-8", errors="surrogateescape") as fh:
                texts[p] = fh.read()
            for line in texts[p].splitlines() if kind_of(p) == "jsonl" else [texts[p]]:
                try:
                    anon.learn(json.loads(line))
                except ValueError:
                    pass
    written = []
    for p in entries:
        rel = os.path.relpath(p, src)
        out_rel = "/".join(anon.map_file_segment(seg) for seg in rel.split(os.sep))
        out = os.path.join(dst, out_rel)
        os.makedirs(os.path.dirname(out), exist_ok=True)
        k = kind_of(p)
        if k == "db":
            anonymize_db(anon, p, out)
        else:
            body = anonymize_jsonl_text(anon, texts[p], max_bytes) if k == "jsonl" else anonymize_json_text(anon, texts[p])
            with open(out, "w", encoding="utf-8", errors="surrogateescape") as fh:
                fh.write(body)
        written.append(out)
        print("%s -> %s" % (rel, out_rel), file=sys.stderr)
    fix_codex_history_base(written)


def process_file(anon, src, dst, max_bytes):
    k = kind_of(src)
    if k == "db":
        anonymize_db(anon, src, dst)
        return
    with open(src, encoding="utf-8", errors="surrogateescape") as fh:
        text = fh.read()
    for line in text.splitlines() if k == "jsonl" else [text]:
        try:
            anon.learn(json.loads(line))
        except ValueError:
            pass
    body = anonymize_jsonl_text(anon, text, max_bytes) if k == "jsonl" else anonymize_json_text(anon, text)
    os.makedirs(os.path.dirname(os.path.abspath(dst)), exist_ok=True)
    with open(dst, "w", encoding="utf-8", errors="surrogateescape") as fh:
        fh.write(body)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src")
    ap.add_argument("dst")
    ap.add_argument("--salt", default=os.environ.get("TBT_ANON_SALT", DEFAULT_SALT))
    ap.add_argument("--text-cap", type=int, default=160, help="max length of replacement text")
    ap.add_argument("--max-bytes", type=int, default=0, help="trim each JSONL file to about this size, keeping usage lines")
    ap.add_argument("--user", action="append", default=[], help="user name to scrub (default: current user)")
    ap.add_argument("--host", action="append", default=[], help="host name to scrub (default: this host)")
    ap.add_argument("--scrub", action="append", default=[], help="extra word (e.g. project name) to replace anywhere")
    args = ap.parse_args(argv)
    users = args.user or [getpass.getuser()]
    hosts = args.host or [socket.gethostname(), socket.gethostname().split(".")[0]]
    anon = Anonymizer(args.salt, args.text_cap, users, hosts, args.scrub)
    if os.path.isdir(args.src):
        process_dir(anon, args.src, args.dst, args.max_bytes)
    else:
        process_file(anon, args.src, args.dst, args.max_bytes)


if __name__ == "__main__":
    main()
