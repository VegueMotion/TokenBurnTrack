//! Incremental line reader for append-only log files (JSONL).
//!
//! Only complete lines (terminated by `\n`) are consumed; a trailing partial line is left
//! for the next run. The source is re-read from 0 when it was truncated, rotated (inode
//! changed) or rewritten (the last consumed line no longer matches its hash).

use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{Cursor, Error, Result};

/// Cursor state for a line-oriented file. Adapters that need more state embed it with
/// `#[serde(flatten)]` and convert via [`Cursor::from_state`] / [`Cursor::state_or_default`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileCursor {
    /// Start of the first unconsumed byte.
    pub byte_offset: u64,
    /// Unix inode; `None` on other platforms.
    pub inode: Option<u64>,
    /// File size when last read.
    pub size: u64,
    /// File mtime when last read; `None` if the last read stopped before the end.
    pub mtime: Option<Timestamp>,
    /// blake3 hex of the last consumed line, including its `\n`.
    pub last_line_hash: Option<String>,
    pub last_line_start: u64,
    /// Number of lines consumed so far (blank lines included).
    pub line_no: u64,
}

impl FileCursor {
    pub fn from_cursor(cursor: &Cursor) -> Result<FileCursor> {
        cursor.state_or_default()
    }

    pub fn to_cursor(&self) -> Result<Cursor> {
        Cursor::from_state(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStart {
    /// Empty cursor: reading from the start.
    Fresh,
    /// Continuing at the cursor's byte offset.
    Continue,
    /// Size and mtime unchanged: nothing to read.
    Unchanged,
    /// The cursor no longer fits the file; reading from the start.
    Restarted(RestartReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartReason {
    /// Size is below the cursor's offset.
    Truncated,
    /// Inode changed.
    Rotated,
    /// Content before the offset changed.
    Rewritten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlLine {
    /// Byte offset of the line start.
    pub offset: u64,
    /// 1-based line number.
    pub line_no: u64,
    /// Line without the trailing `\n` / `\r\n`; invalid UTF-8 is replaced lossily.
    pub text: String,
}

/// Iterator over the new complete, non-blank lines of a file.
/// Call [`JsonlReader::into_cursor`] afterwards to get the cursor to save.
pub struct JsonlReader {
    path: PathBuf,
    reader: Option<io::Take<BufReader<File>>>,
    start: ReadStart,
    next: FileCursor,
    observed_mtime: Option<Timestamp>,
    finished: bool,
    buf: Vec<u8>,
}

impl JsonlReader {
    pub fn open(path: &Path, cursor: &FileCursor) -> Result<JsonlReader> {
        let mut file = File::open(path).map_err(|e| Error::io(path, e))?;
        let meta = file.metadata().map_err(|e| Error::io(path, e))?;
        let size = meta.len();
        let inode = inode_of(&meta);
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok());

        let start =
            classify(&mut file, cursor, size, inode, mtime).map_err(|e| Error::io(path, e))?;

        let mut next = match start {
            ReadStart::Continue | ReadStart::Unchanged => cursor.clone(),
            ReadStart::Fresh | ReadStart::Restarted(_) => FileCursor::default(),
        };
        next.inode = inode;
        next.size = size;
        next.mtime = None;

        if start == ReadStart::Unchanged {
            next.mtime = mtime;
            return Ok(JsonlReader {
                path: path.to_owned(),
                reader: None,
                start,
                next,
                observed_mtime: mtime,
                finished: true,
                buf: Vec::new(),
            });
        }

        file.seek(SeekFrom::Start(next.byte_offset))
            .map_err(|e| Error::io(path, e))?;
        let remaining = size.saturating_sub(next.byte_offset);
        Ok(JsonlReader {
            path: path.to_owned(),
            reader: Some(BufReader::new(file).take(remaining)),
            start,
            next,
            observed_mtime: mtime,
            finished: false,
            buf: Vec::new(),
        })
    }

    pub fn start(&self) -> ReadStart {
        self.start
    }

    /// Cursor covering the lines yielded so far.
    pub fn cursor(&self) -> FileCursor {
        let mut c = self.next.clone();
        if self.finished {
            c.mtime = self.observed_mtime;
        }
        c
    }

    pub fn into_cursor(self) -> FileCursor {
        self.cursor()
    }

    fn read_line(&mut self) -> Result<Option<JsonlLine>> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };
        loop {
            self.buf.clear();
            let n = reader
                .read_until(b'\n', &mut self.buf)
                .map_err(|e| Error::io(&self.path, e))?;
            if n == 0 || self.buf.last() != Some(&b'\n') {
                self.finished = true;
                self.reader = None;
                return Ok(None);
            }
            let offset = self.next.byte_offset;
            self.next.byte_offset += n as u64;
            self.next.line_no += 1;
            self.next.last_line_start = offset;
            self.next.last_line_hash = Some(blake3::hash(&self.buf).to_hex().to_string());

            let mut content = &self.buf[..n - 1];
            if content.last() == Some(&b'\r') {
                content = &content[..content.len() - 1];
            }
            if content.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            return Ok(Some(JsonlLine {
                offset,
                line_no: self.next.line_no,
                text: String::from_utf8_lossy(content).into_owned(),
            }));
        }
    }
}

impl Iterator for JsonlReader {
    type Item = Result<JsonlLine>;

    fn next(&mut self) -> Option<Self::Item> {
        self.read_line().transpose()
    }
}

/// Result of [`read_new_lines`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlRead {
    pub start: ReadStart,
    pub lines: Vec<JsonlLine>,
    pub next: FileCursor,
}

/// Reads all new complete lines at once.
pub fn read_new_lines(path: &Path, cursor: &FileCursor) -> Result<JsonlRead> {
    let mut reader = JsonlReader::open(path, cursor)?;
    let lines = reader.by_ref().collect::<Result<Vec<_>>>()?;
    Ok(JsonlRead {
        start: reader.start(),
        lines,
        next: reader.into_cursor(),
    })
}

fn classify(
    file: &mut File,
    cursor: &FileCursor,
    size: u64,
    inode: Option<u64>,
    mtime: Option<Timestamp>,
) -> io::Result<ReadStart> {
    if *cursor == FileCursor::default() {
        return Ok(ReadStart::Fresh);
    }
    if matches!((cursor.inode, inode), (Some(a), Some(b)) if a != b) {
        return Ok(ReadStart::Restarted(RestartReason::Rotated));
    }
    if size < cursor.byte_offset {
        return Ok(ReadStart::Restarted(RestartReason::Truncated));
    }
    if cursor.mtime.is_some() && cursor.mtime == mtime && cursor.size == size {
        return Ok(ReadStart::Unchanged);
    }
    if let Some(expected) = &cursor.last_line_hash
        && !last_line_matches(file, cursor, expected)?
    {
        return Ok(ReadStart::Restarted(RestartReason::Rewritten));
    }
    Ok(ReadStart::Continue)
}

fn last_line_matches(file: &mut File, cursor: &FileCursor, expected: &str) -> io::Result<bool> {
    if cursor.last_line_start > cursor.byte_offset {
        return Ok(false);
    }
    let len = cursor.byte_offset - cursor.last_line_start;
    file.seek(SeekFrom::Start(cursor.last_line_start))?;
    let mut hasher = blake3::Hasher::new();
    let copied = io::copy(&mut file.by_ref().take(len), &mut hasher)?;
    Ok(copied == len && hasher.finalize().to_hex().as_str() == expected)
}

#[cfg(unix)]
fn inode_of(meta: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn inode_of(_meta: &Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn write(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }

    fn append(path: &Path, content: &str) {
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn texts(r: &JsonlRead) -> Vec<&str> {
        r.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn fresh_then_append() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "{\"n\":1}\n{\"n\":2}\n");

        let r1 = read_new_lines(&p, &FileCursor::default()).unwrap();
        assert_eq!(r1.start, ReadStart::Fresh);
        assert_eq!(texts(&r1), ["{\"n\":1}", "{\"n\":2}"]);
        assert_eq!(r1.lines[1].offset, 8);
        assert_eq!(r1.lines[1].line_no, 2);
        assert_eq!(r1.next.byte_offset, 16);
        assert_eq!(r1.next.size, 16);
        assert!(r1.next.mtime.is_some());

        append(&p, "{\"n\":3}\n");
        let r2 = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(r2.start, ReadStart::Continue);
        assert_eq!(texts(&r2), ["{\"n\":3}"]);
        assert_eq!(r2.lines[0].offset, 16);
        assert_eq!(r2.lines[0].line_no, 3);
        assert_eq!(r2.next.byte_offset, 24);
    }

    #[test]
    fn partial_line_is_left_for_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "{\"n\":1}\n{\"n\":");

        let r1 = read_new_lines(&p, &FileCursor::default()).unwrap();
        assert_eq!(texts(&r1), ["{\"n\":1}"]);
        assert_eq!(r1.next.byte_offset, 8);
        assert_eq!(r1.next.size, 13);

        let again = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(again.start, ReadStart::Unchanged);
        assert!(again.lines.is_empty());

        append(&p, "2}\n");
        let r2 = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(r2.start, ReadStart::Continue);
        assert_eq!(texts(&r2), ["{\"n\":2}"]);
        assert_eq!(r2.lines[0].offset, 8);
        assert_eq!(r2.next.byte_offset, 16);
    }

    #[test]
    fn unchanged_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "a\nb\n");
        let r1 = read_new_lines(&p, &FileCursor::default()).unwrap();
        let r2 = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(r2.start, ReadStart::Unchanged);
        assert!(r2.lines.is_empty());
        assert_eq!(r2.next, r1.next);
    }

    #[test]
    fn truncation_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "first line\nsecond line\n");
        let r1 = read_new_lines(&p, &FileCursor::default()).unwrap();

        write(&p, "x\n");
        let r2 = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(r2.start, ReadStart::Restarted(RestartReason::Truncated));
        assert_eq!(texts(&r2), ["x"]);
        assert_eq!(r2.lines[0].line_no, 1);
        assert_eq!(r2.next.byte_offset, 2);
    }

    #[cfg(unix)]
    #[test]
    fn rotation_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "old 1\n");
        let r1 = read_new_lines(&p, &FileCursor::default()).unwrap();

        let tmp = dir.path().join("new.jsonl");
        write(&tmp, "new 1\nnew 2\n");
        std::fs::rename(&tmp, &p).unwrap();

        let r2 = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(r2.start, ReadStart::Restarted(RestartReason::Rotated));
        assert_eq!(texts(&r2), ["new 1", "new 2"]);
        assert_ne!(r2.next.inode, r1.next.inode);
    }

    #[test]
    fn rewrite_in_place_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "aaa\nbbb\n");
        let r1 = read_new_lines(&p, &FileCursor::default()).unwrap();

        write(&p, "aaa\nccc\nddd\n");
        let r2 = read_new_lines(&p, &r1.next).unwrap();
        assert_eq!(r2.start, ReadStart::Restarted(RestartReason::Rewritten));
        assert_eq!(texts(&r2), ["aaa", "ccc", "ddd"]);
    }

    #[test]
    fn blank_lines_and_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "a\r\n\n  \nb\n");
        let r = read_new_lines(&p, &FileCursor::default()).unwrap();
        assert_eq!(texts(&r), ["a", "b"]);
        assert_eq!(r.lines[1].line_no, 4);
        assert_eq!(r.lines[1].offset, 7);
        assert_eq!(r.next.line_no, 4);
    }

    #[test]
    fn stopping_early_does_not_mark_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "1\n2\n3\n");

        let mut reader = JsonlReader::open(&p, &FileCursor::default()).unwrap();
        let first = reader.next().unwrap().unwrap();
        assert_eq!(first.text, "1");
        let partial = reader.into_cursor();
        assert_eq!(partial.byte_offset, 2);
        assert!(partial.mtime.is_none());

        let rest = read_new_lines(&p, &partial).unwrap();
        assert_eq!(rest.start, ReadStart::Continue);
        assert_eq!(texts(&rest), ["2", "3"]);
    }

    #[test]
    fn cursor_roundtrip_and_flatten() {
        #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
        struct AdapterState {
            #[serde(flatten)]
            file: FileCursor,
            last_total: Option<u64>,
        }

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        write(&p, "x\n");
        let r = read_new_lines(&p, &FileCursor::default()).unwrap();

        let c = r.next.to_cursor().unwrap();
        assert_eq!(FileCursor::from_cursor(&c).unwrap(), r.next);
        assert_eq!(
            FileCursor::from_cursor(&Cursor::empty()).unwrap(),
            FileCursor::default()
        );

        let state = AdapterState {
            file: r.next.clone(),
            last_total: Some(5),
        };
        let c = Cursor::from_state(&state).unwrap();
        assert_eq!(c.as_json()["byte_offset"], 2);
        assert_eq!(c.state::<AdapterState>().unwrap(), Some(state));
    }

    #[test]
    fn missing_file_is_io_error_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.jsonl");
        let err = read_new_lines(&p, &FileCursor::default()).unwrap_err();
        assert!(matches!(err, Error::Io { path: Some(ref x), .. } if x == &p));
    }
}
