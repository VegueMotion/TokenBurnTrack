use std::path::{Path, PathBuf};

use tbt_core::{SourceKind, SourceRef};

pub fn files_with_extension(roots: &[PathBuf], ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        match std::fs::metadata(root) {
            Ok(meta) if meta.is_dir() => walk(root, ext, &mut out),
            Ok(meta) if meta.is_file() && has_extension(root, ext) => out.push(root.clone()),
            _ => {}
        }
    }
    out.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    out.dedup();
    out
}

pub fn sources_with_extension(roots: &[PathBuf], ext: &str, kind: SourceKind) -> Vec<SourceRef> {
    files_with_extension(roots, ext)
        .into_iter()
        .map(|p| SourceRef::new(p, kind))
        .collect()
}

fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "skipping unreadable directory");
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            walk(&path, ext, out);
        } else if has_extension(&path, ext) && std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            out.push(path);
        }
    }
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e == ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_recursively_sorted_and_skips_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("b/sub/deeper")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        for f in ["b/sub/deeper/x.jsonl", "a/z.jsonl", "a/y.json", "b/a.jsonl"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let roots = vec![
            root.to_path_buf(),
            root.join("missing"),
            root.join("a/z.jsonl"),
        ];
        let found = files_with_extension(&roots, "jsonl");
        let rel: Vec<_> = found
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(rel, ["a/z.jsonl", "b/a.jsonl", "b/sub/deeper/x.jsonl"]);

        std::fs::create_dir_all(root.join("c/s1/subagents")).unwrap();
        std::fs::write(root.join("c/s1.jsonl"), "").unwrap();
        std::fs::write(root.join("c/s1/subagents/agent-1.jsonl"), "").unwrap();
        let found = files_with_extension(&[root.join("c")], "jsonl");
        assert!(found[0].ends_with("c/s1.jsonl"));
        assert!(found[1].ends_with("c/s1/subagents/agent-1.jsonl"));
    }
}
