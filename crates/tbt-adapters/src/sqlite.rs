use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode, OpenFlags};

pub(crate) fn open_read_only(path: &Path, immutable: bool) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(read_only_uri(path, immutable), flags)
}

pub(crate) fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

fn read_only_uri(path: &Path, immutable: bool) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '\\' => uri.push('/'),
            '?' | '#' | '%' | ' ' => uri.push_str(&format!("%{:02X}", c as u32)),
            c => uri.push(c),
        }
    }
    uri.push_str("?mode=ro");
    if immutable {
        uri.push_str("&immutable=1");
    }
    uri
}

pub(crate) fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

pub(crate) fn is_read_only(e: &rusqlite::Error) -> bool {
    e.sqlite_error_code() == Some(ErrorCode::ReadOnly)
}

pub(crate) fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_escapes_special_characters() {
        assert_eq!(
            read_only_uri(Path::new("/a b/x?y#z%.db"), false),
            "file:/a%20b/x%3Fy%23z%25.db?mode=ro"
        );
        assert_eq!(
            read_only_uri(Path::new("/d/opencode.db"), true),
            "file:/d/opencode.db?mode=ro&immutable=1"
        );
    }
}
