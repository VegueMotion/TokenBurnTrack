//! Maps a git worktree path to the main checkout it belongs to, so a session run
//! in a worktree counts toward the main project instead of a project of its own.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// The main checkout path for `path` when `path` lies in a git worktree, with any
/// subdirectory below the worktree root kept. `None` when `path` is not in a
/// worktree, or when the main checkout cannot be determined.
///
/// A worktree that still exists is resolved from its `.git` file. One that was
/// already deleted is recognized by its location: `<repo>/.claude/worktrees/<name>`
/// maps to `<repo>`, and `…/worktrees/<repo name>/<name>` maps to the one known
/// project named `<repo name>`, looked up with `known`.
pub(crate) fn main_checkout(
    path: &str,
    known: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<String> {
    let p = Path::new(path);
    let main = if p.exists() {
        from_git(p)?
    } else {
        from_layout(p, known)?
    };
    let main = main.to_str()?.to_owned();
    (main != path).then_some(main)
}

fn from_git(path: &Path) -> Option<PathBuf> {
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
        let rest = path.strip_prefix(root).ok()?;
        return Some(join(common.parent()?, rest));
    }
    None
}

fn from_layout(path: &Path, known: &mut dyn FnMut(&str) -> Option<String>) -> Option<PathBuf> {
    let comps: Vec<Component> = path.components().collect();
    let at = comps.iter().position(|c| c.as_os_str() == "worktrees")?;
    if at >= 1 && comps[at - 1].as_os_str() == ".claude" && comps.len() > at + 1 {
        let main: PathBuf = comps[..at - 1].iter().collect();
        let rest: PathBuf = comps[at + 2..].iter().collect();
        return Some(join(&main, &rest));
    }
    if comps.len() > at + 2 {
        let main = PathBuf::from(known(comps[at + 1].as_os_str().to_str()?)?);
        let rest: PathBuf = comps[at + 3..].iter().collect();
        return Some(join(&main, &rest));
    }
    None
}

fn join(main: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        main.to_path_buf()
    } else {
        main.join(rest)
    }
}

/// Resolves `.` and `..` without touching the filesystem, so symlinks in the
/// recorded path are kept as the harness logged them.
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

    fn none(_: &str) -> Option<String> {
        None
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
        (tmp, main, wt)
    }

    #[test]
    fn existing_worktree_maps_to_main_checkout() {
        let (_tmp, main, wt) = repo_with_worktree();
        assert_eq!(main_checkout(&s(&wt), &mut none), Some(s(&main)));
        assert_eq!(
            main_checkout(&s(&wt.join("src")), &mut none),
            Some(s(&main.join("src")))
        );
    }

    #[test]
    fn main_checkout_and_plain_dirs_are_unchanged() {
        let (tmp, main, _wt) = repo_with_worktree();
        assert_eq!(main_checkout(&s(&main), &mut none), None);
        assert_eq!(main_checkout(&s(tmp.path()), &mut none), None);
    }

    #[test]
    fn submodule_is_not_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let modules = tmp.path().join("repo/.git/modules/sub");
        fs::create_dir_all(&modules).unwrap();
        let sub = tmp.path().join("repo/sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
        assert_eq!(main_checkout(&s(&sub), &mut none), None);
    }

    #[test]
    fn deleted_claude_worktree_maps_by_layout() {
        assert_eq!(
            main_checkout("/nonexistent/p/App/.claude/worktrees/issue-1", &mut none),
            Some("/nonexistent/p/App".into())
        );
        assert_eq!(
            main_checkout(
                "/nonexistent/p/App/.claude/worktrees/issue-1/crates/x",
                &mut none
            ),
            Some("/nonexistent/p/App/crates/x".into())
        );
    }

    #[test]
    fn deleted_external_worktree_maps_to_known_project_by_name() {
        let mut known = |name: &str| (name == "App").then(|| "/nonexistent/p/App".to_owned());
        assert_eq!(
            main_checkout("/nonexistent/.herdr/worktrees/App/worker-3", &mut known),
            Some("/nonexistent/p/App".into())
        );
        assert_eq!(
            main_checkout("/nonexistent/.herdr/worktrees/Other/worker-3", &mut known),
            None
        );
        assert_eq!(main_checkout("/nonexistent/p/App", &mut known), None);
    }
}
