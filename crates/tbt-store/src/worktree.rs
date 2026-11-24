//! Maps a git worktree path to the main checkout it belongs to, so a session run
//! in a worktree counts toward the main project instead of a project of its own.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::api::Result;

/// What the resolver needs to know about stored data.
pub(crate) trait Lookup {
    /// Recorded `(worktree root, main checkout)` pairs.
    fn recorded(&mut self) -> Result<Vec<(String, String)>>;
    /// Stored project paths with this directory name.
    fn named(&mut self, name: &str) -> Result<Vec<String>>;
    /// All stored project paths.
    fn projects(&mut self) -> Result<Vec<String>>;
}

/// Resolves worktree paths to main checkouts. Worktrees seen while they still
/// exist are collected in `learned`, so the caller can record them and resolve
/// the same paths after the worktree is deleted.
pub(crate) struct Resolver<L> {
    lookup: L,
    recorded: Option<Vec<(String, String)>>,
    linked: Option<Vec<(PathBuf, String)>>,
    pub(crate) learned: Vec<(String, String)>,
}

/// Bounds re-resolution, e.g. a Claude Code worktree inside another worktree.
const MAX_HOPS: usize = 4;

impl<L: Lookup> Resolver<L> {
    pub(crate) fn new(lookup: L) -> Self {
        Resolver {
            lookup,
            recorded: None,
            linked: None,
            learned: Vec::new(),
        }
    }

    /// The main checkout path for `path` when `path` lies in a git worktree, with
    /// any subdirectory below the worktree root kept. `None` when `path` is not in
    /// a worktree, or when the main checkout cannot be determined.
    ///
    /// A worktree that still exists is resolved from its `.git` file. One that was
    /// already deleted is resolved from a mapping recorded while it existed, from
    /// the `<repo>/.claude/worktrees/<name>` layout, or from a stored project whose
    /// `.git/worktrees/*/gitdir` still points at it. Nothing is guessed by name.
    pub(crate) fn main_checkout(&mut self, path: &str) -> Result<Option<String>> {
        let mut current = path.to_owned();
        for _ in 0..MAX_HOPS {
            match self.step(&current)? {
                Some(next) if next != current => current = next,
                _ => break,
            }
        }
        Ok((current != path).then_some(current))
    }

    fn step(&mut self, path: &str) -> Result<Option<String>> {
        let p = Path::new(path);
        let main = if p.exists() {
            self.via_git(p)?
        } else if let Some(main) = self.via_recorded(p)? {
            Some(main)
        } else if let Some(main) = from_claude_layout(p) {
            Some(main)
        } else {
            self.via_linked(p)?
        };
        Ok(main.and_then(|m| m.to_str().map(str::to_owned)))
    }

    fn via_git(&mut self, path: &Path) -> Result<Option<PathBuf>> {
        let Some((root, main)) = git_main(path) else {
            return Ok(None);
        };
        let main = self.stored_spelling(&main)?;
        if let (Some(r), Some(m)) = (root.to_str(), main.to_str()) {
            let pair = (r.to_owned(), m.to_owned());
            if !self.learned.contains(&pair) {
                self.learned.push(pair);
            }
        }
        let Ok(rest) = path.strip_prefix(&root) else {
            return Ok(None);
        };
        Ok(Some(join(&main, rest)))
    }

    /// `main` as derived from git metadata has symlinks resolved, while harnesses
    /// log paths as typed. Prefer a stored project that is the same directory.
    fn stored_spelling(&mut self, main: &Path) -> Result<PathBuf> {
        let (Some(name), Ok(canon)) = (
            main.file_name().and_then(|n| n.to_str()),
            fs::canonicalize(main),
        ) else {
            return Ok(main.to_path_buf());
        };
        for stored in self.lookup.named(name)? {
            if Path::new(&stored) != main && fs::canonicalize(&stored).ok() == Some(canon.clone()) {
                return Ok(PathBuf::from(stored));
            }
        }
        Ok(main.to_path_buf())
    }

    fn via_recorded(&mut self, path: &Path) -> Result<Option<PathBuf>> {
        if self.recorded.is_none() {
            self.recorded = Some(self.lookup.recorded()?);
        }
        let best = self
            .recorded
            .iter()
            .flatten()
            .chain(self.learned.iter())
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.len());
        Ok(best.and_then(|(root, main)| {
            let rest = path.strip_prefix(root).ok()?;
            Some(join(Path::new(main), rest))
        }))
    }

    fn via_linked(&mut self, path: &Path) -> Result<Option<PathBuf>> {
        if self.linked.is_none() {
            let mut linked = Vec::new();
            for project in self.lookup.projects()? {
                linked
                    .extend(linked_worktrees(Path::new(&project)).map(|wt| (wt, project.clone())));
            }
            self.linked = Some(linked);
        }
        let real = canonical_lossy(path);
        for candidate in [path, real.as_path()] {
            let best = self
                .linked
                .iter()
                .flatten()
                .filter(|(wt, _)| candidate.starts_with(wt))
                .max_by_key(|(wt, _)| wt.as_os_str().len());
            if let Some((wt, main)) = best
                && let Ok(rest) = candidate.strip_prefix(wt)
            {
                return Ok(Some(join(Path::new(main), rest)));
            }
        }
        Ok(None)
    }
}

/// The worktree root and main checkout for an existing `path` in a linked worktree.
fn git_main(path: &Path) -> Option<(PathBuf, PathBuf)> {
    for root in path.ancestors() {
        let dotgit = root.join(".git");
        let Ok(meta) = fs::metadata(&dotgit) else {
            continue;
        };
        if !meta.is_file() {
            return None;
        }
        let text = fs::read_to_string(&dotgit).ok()?;
        let gitdir = root.join(text.lines().next()?.strip_prefix("gitdir:")?.trim());
        // Submodules also use a `.git` file but have no `commondir`.
        let common = fs::read_to_string(gitdir.join("commondir")).ok()?;
        let common = normalize(&gitdir.join(common.trim()));
        if common.file_name()? != ".git" {
            return None;
        }
        return Some((root.to_path_buf(), common.parent()?.to_path_buf()));
    }
    None
}

/// `<repo>/.claude/worktrees/<name>` maps to `<repo>`. The last such segment is
/// used; anything above it is resolved again by the caller.
fn from_claude_layout(path: &Path) -> Option<PathBuf> {
    let comps: Vec<Component> = path.components().collect();
    let at = (1..comps.len().saturating_sub(1))
        .rev()
        .find(|&i| comps[i].as_os_str() == "worktrees" && comps[i - 1].as_os_str() == ".claude")?;
    let main: PathBuf = comps[..at - 1].iter().collect();
    let rest: PathBuf = comps[at + 2..].iter().collect();
    Some(join(&main, &rest))
}

/// Worktree roots that git still lists for the main checkout at `main`.
fn linked_worktrees(main: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(main.join(".git").join("worktrees"))
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let text = fs::read_to_string(entry.ok()?.path().join("gitdir")).ok()?;
            let dotgit = PathBuf::from(text.trim());
            (dotgit.file_name()? == ".git").then(|| dotgit.parent().map(Path::to_path_buf))?
        })
}

/// `path` with symlinks resolved as far as it still exists.
fn canonical_lossy(path: &Path) -> PathBuf {
    for base in path.ancestors() {
        if let Ok(real) = fs::canonicalize(base) {
            return match path.strip_prefix(base) {
                Ok(rest) => join(&real, rest),
                Err(_) => real,
            };
        }
    }
    path.to_path_buf()
}

fn join(main: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        main.to_path_buf()
    } else {
        main.join(rest)
    }
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        recorded: Vec<(String, String)>,
        projects: Vec<String>,
    }

    impl Lookup for &Fake {
        fn recorded(&mut self) -> Result<Vec<(String, String)>> {
            Ok(self.recorded.clone())
        }
        fn named(&mut self, name: &str) -> Result<Vec<String>> {
            Ok(self
                .projects
                .iter()
                .filter(|p| Path::new(p).file_name().and_then(|n| n.to_str()) == Some(name))
                .cloned()
                .collect())
        }
        fn projects(&mut self) -> Result<Vec<String>> {
            Ok(self.projects.clone())
        }
    }

    fn resolve(fake: &Fake, path: &str) -> Option<String> {
        Resolver::new(fake).main_checkout(path).unwrap()
    }

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    /// A main checkout at `<tmp>/repo` and a linked worktree at `<tmp>/wt/feature`,
    /// laid out the way `git worktree add` writes them.
    fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("repo");
        let admin = main.join(".git/worktrees/feature");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        let wt = tmp.path().join("wt/feature");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        fs::write(
            admin.join("gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .unwrap();
        (tmp, main, wt)
    }

    #[test]
    fn existing_worktree_maps_to_main_checkout_and_is_learned() {
        let (_tmp, main, wt) = repo_with_worktree();
        let fake = Fake::default();
        let mut r = Resolver::new(&fake);
        assert_eq!(r.main_checkout(&s(&wt)).unwrap(), Some(s(&main)));
        assert_eq!(
            r.main_checkout(&s(&wt.join("src"))).unwrap(),
            Some(s(&main.join("src")))
        );
        assert_eq!(r.learned, [(s(&wt), s(&main))]);
    }

    #[test]
    fn main_checkout_and_plain_dirs_are_unchanged() {
        let (tmp, main, _wt) = repo_with_worktree();
        let fake = Fake::default();
        assert_eq!(resolve(&fake, &s(&main)), None);
        assert_eq!(resolve(&fake, &s(tmp.path())), None);
    }

    #[test]
    fn submodule_is_not_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let modules = tmp.path().join("repo/.git/modules/sub");
        fs::create_dir_all(&modules).unwrap();
        let sub = tmp.path().join("repo/sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
        assert_eq!(resolve(&Fake::default(), &s(&sub)), None);
    }

    #[cfg(unix)]
    #[test]
    fn main_checkout_keeps_the_stored_symlinked_spelling() {
        let (tmp, main, wt) = repo_with_worktree();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(tmp.path(), &link).unwrap();
        let stored = s(&link.join("repo"));
        let fake = Fake {
            projects: vec![stored.clone()],
            ..Fake::default()
        };
        assert_ne!(stored, s(&main));
        assert_eq!(resolve(&fake, &s(&wt)), Some(stored));
    }

    #[test]
    fn deleted_claude_worktree_maps_by_layout() {
        let fake = Fake::default();
        assert_eq!(
            resolve(&fake, "/nonexistent/p/App/.claude/worktrees/issue-1"),
            Some("/nonexistent/p/App".into())
        );
        assert_eq!(
            resolve(
                &fake,
                "/nonexistent/p/App/.claude/worktrees/issue-1/crates/x"
            ),
            Some("/nonexistent/p/App/crates/x".into())
        );
    }

    #[test]
    fn deleted_nested_worktree_resolves_to_the_main_checkout() {
        let fake = Fake {
            recorded: vec![(
                "/nonexistent/.herdr/worktrees/App/w1".into(),
                "/nonexistent/p/App".into(),
            )],
            ..Fake::default()
        };
        assert_eq!(
            resolve(
                &fake,
                "/nonexistent/.herdr/worktrees/App/w1/.claude/worktrees/foo/src"
            ),
            Some("/nonexistent/p/App/src".into())
        );
    }

    #[test]
    fn deleted_worktree_maps_by_recorded_mapping_only() {
        let fake = Fake {
            recorded: vec![(
                "/nonexistent/.herdr/worktrees/App/w1".into(),
                "/nonexistent/p/App".into(),
            )],
            projects: vec!["/nonexistent/p/App".into(), "/nonexistent/q/Other".into()],
        };
        assert_eq!(
            resolve(&fake, "/nonexistent/.herdr/worktrees/App/w1/src"),
            Some("/nonexistent/p/App/src".into())
        );
        // Same repo name, but no evidence it belongs to the stored project.
        assert_eq!(resolve(&fake, "/nonexistent/.herdr/worktrees/App/w2"), None);
        assert_eq!(resolve(&fake, "/data/worktrees/Other/tmp"), None);
        assert_eq!(resolve(&fake, "/nonexistent/p/App"), None);
    }

    #[test]
    fn deleted_worktree_maps_by_git_admin_dir() {
        let (_tmp, main, wt) = repo_with_worktree();
        fs::remove_dir_all(&wt).unwrap();
        let fake = Fake {
            projects: vec![s(&main)],
            ..Fake::default()
        };
        assert_eq!(
            resolve(&fake, &s(&wt.join("src"))),
            Some(s(&main.join("src")))
        );
    }
}
