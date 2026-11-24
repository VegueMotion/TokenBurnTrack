use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    AnyDirs,
    Name(Vec<Tok>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Star,
    One,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    segs: Vec<Seg>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid glob `{glob}`: {message}")]
pub struct GlobError {
    pub glob: String,
    pub message: String,
}

impl Glob {
    pub fn parse(src: &str) -> Result<Glob, GlobError> {
        let err = |message: &str| GlobError {
            glob: src.to_owned(),
            message: message.to_owned(),
        };
        if src.trim().is_empty() {
            return Err(err("empty pattern"));
        }
        if src.starts_with('/') || src.starts_with('\\') {
            return Err(err(
                "the pattern is matched relative to each root and must not start with `/`",
            ));
        }
        if src.contains(['[', ']', '{', '}']) {
            return Err(err("only `*`, `?` and `**` are supported"));
        }
        let mut segs = Vec::new();
        for part in src.split(['/', '\\']) {
            match part {
                "" => return Err(err("empty path segment")),
                "**" => {
                    if segs.last() != Some(&Seg::AnyDirs) {
                        segs.push(Seg::AnyDirs);
                    }
                }
                _ if part.contains("**") => {
                    return Err(err("`**` must be a whole path segment"));
                }
                _ => segs.push(Seg::Name(
                    part.chars()
                        .map(|c| match c {
                            '*' => Tok::Star,
                            '?' => Tok::One,
                            c => Tok::Lit(c),
                        })
                        .collect(),
                )),
            }
        }
        if segs.last() == Some(&Seg::AnyDirs) {
            return Err(err(
                "the pattern must end with a file name pattern, e.g. `**/*.jsonl`",
            ));
        }
        Ok(Glob { segs })
    }

    pub fn is_match(&self, rel: &Path) -> bool {
        parts(rel).is_some_and(|p| match_segs(&self.segs, &p))
    }

    fn dir_may_match(&self, rel: &Path) -> bool {
        parts(rel).is_some_and(|p| prefix_may_match(&self.segs, &p))
    }

    pub fn files(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in roots {
            match std::fs::metadata(root) {
                Ok(meta) if meta.is_dir() => self.walk(root, root, &mut out),
                Ok(meta) if meta.is_file() => out.push(root.clone()),
                _ => {}
            }
        }
        out.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
        out.dedup();
        out
    }

    fn walk(&self, root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
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
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            if file_type.is_dir() {
                if self.dir_may_match(rel) {
                    self.walk(root, &path, out);
                }
            } else if self.is_match(rel) && std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                out.push(path);
            }
        }
    }
}

fn parts(rel: &Path) -> Option<Vec<Vec<char>>> {
    rel.components()
        .map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().chars().collect()),
            _ => None,
        })
        .collect()
}

fn match_segs(segs: &[Seg], parts: &[Vec<char>]) -> bool {
    match segs.split_first() {
        None => parts.is_empty(),
        Some((Seg::AnyDirs, rest)) => (0..=parts.len()).any(|i| match_segs(rest, &parts[i..])),
        Some((Seg::Name(toks), rest)) => parts
            .split_first()
            .is_some_and(|(first, tail)| match_name(toks, first) && match_segs(rest, tail)),
    }
}

fn prefix_may_match(segs: &[Seg], parts: &[Vec<char>]) -> bool {
    match (segs.split_first(), parts.split_first()) {
        (_, None) => !segs.is_empty(),
        (None, Some(_)) => false,
        (Some((Seg::AnyDirs, _)), _) => true,
        (Some((Seg::Name(toks), rest)), Some((first, tail))) => {
            match_name(toks, first) && prefix_may_match(rest, tail)
        }
    }
}

fn match_name(toks: &[Tok], name: &[char]) -> bool {
    match toks.split_first() {
        None => name.is_empty(),
        Some((Tok::Star, rest)) => (0..=name.len()).any(|i| match_name(rest, &name[i..])),
        Some((Tok::One, rest)) => !name.is_empty() && match_name(rest, &name[1..]),
        Some((Tok::Lit(c), rest)) => name.first() == Some(c) && match_name(rest, &name[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(glob: &str, path: &str) -> bool {
        Glob::parse(glob).unwrap().is_match(Path::new(path))
    }

    #[test]
    fn matches() {
        assert!(m("**/*.jsonl", "a.jsonl"));
        assert!(m("**/*.jsonl", "x/y/z/a.jsonl"));
        assert!(!m("**/*.jsonl", "x/a.json"));
        assert!(m("*.jsonl", "a.jsonl"));
        assert!(!m("*.jsonl", "x/a.jsonl"));
        assert!(m("logs/**/usage-*.log", "logs/usage-1.log"));
        assert!(m("logs/**/usage-*.log", "logs/2026/11/usage-1.log"));
        assert!(!m("logs/**/usage-*.log", "other/usage-1.log"));
        assert!(m("*/session-??.jsonl", "proj/session-01.jsonl"));
        assert!(!m("*/session-??.jsonl", "proj/session-1.jsonl"));
        assert!(m("a/**/**/b.jsonl", "a/b.jsonl"));
    }

    #[test]
    fn prunes_directories() {
        let g = Glob::parse("logs/*/usage.jsonl").unwrap();
        assert!(g.dir_may_match(Path::new("logs")));
        assert!(g.dir_may_match(Path::new("logs/x")));
        assert!(!g.dir_may_match(Path::new("logs/x/y")));
        assert!(!g.dir_may_match(Path::new("other")));
        let g = Glob::parse("**/*.jsonl").unwrap();
        assert!(g.dir_may_match(Path::new("a/b/c")));
    }

    #[test]
    fn errors() {
        for (glob, message) in [
            ("", "empty pattern"),
            (
                "/abs/*.jsonl",
                "the pattern is matched relative to each root and must not start with `/`",
            ),
            ("logs/[ab].jsonl", "only `*`, `?` and `**` are supported"),
            ("a//b", "empty path segment"),
            ("a**/b", "`**` must be a whole path segment"),
            (
                "logs/**",
                "the pattern must end with a file name pattern, e.g. `**/*.jsonl`",
            ),
        ] {
            let e = Glob::parse(glob).unwrap_err();
            assert_eq!(e.message, message, "{glob}");
        }
    }

    #[test]
    fn walks_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "a/x.jsonl",
            "a/b/y.jsonl",
            "a/b/y.json",
            "c.jsonl",
            "skip/z.jsonl",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let rel = |g: &str, roots: &[PathBuf]| -> Vec<String> {
            Glob::parse(g)
                .unwrap()
                .files(roots)
                .iter()
                .map(|p| {
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect()
        };
        let roots = [root.to_path_buf(), root.join("missing")];
        assert_eq!(
            rel("**/*.jsonl", &roots),
            ["a/b/y.jsonl", "a/x.jsonl", "c.jsonl", "skip/z.jsonl"]
        );
        assert_eq!(rel("a/**/*.jsonl", &roots), ["a/b/y.jsonl", "a/x.jsonl"]);
        assert_eq!(
            rel("*.jsonl", &[root.to_path_buf(), root.join("a/b/y.json")]),
            ["a/b/y.json", "c.jsonl"]
        );
    }
}
