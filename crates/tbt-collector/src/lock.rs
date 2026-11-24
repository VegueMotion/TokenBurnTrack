use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const LOCK_FILE: &str = "collector.lock";
pub const DAEMON_LOCK_FILE: &str = "daemon.lock";

pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOCK_FILE)
}

#[derive(Debug)]
pub struct CollectorLock {
    file: File,
    path: PathBuf,
}

#[derive(Debug)]
pub enum LockAttempt {
    Acquired(CollectorLock),
    Busy,
}

impl LockAttempt {
    pub fn is_busy(&self) -> bool {
        matches!(self, LockAttempt::Busy)
    }
}

impl CollectorLock {
    pub fn try_acquire(data_dir: &Path) -> std::io::Result<LockAttempt> {
        Self::try_acquire_file(data_dir, LOCK_FILE)
    }

    pub fn try_acquire_daemon(data_dir: &Path) -> std::io::Result<LockAttempt> {
        Self::try_acquire_file(data_dir, DAEMON_LOCK_FILE)
    }

    fn try_acquire_file(data_dir: &Path, name: &str) -> std::io::Result<LockAttempt> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(name);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => return Ok(LockAttempt::Busy),
            Err(fs4::TryLockError::Error(e)) => return Err(e),
        }
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        Ok(LockAttempt::Acquired(CollectorLock { file, path }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CollectorLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_attempt_is_busy_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let LockAttempt::Acquired(lock) = CollectorLock::try_acquire(&data).unwrap() else {
            panic!("first attempt should acquire");
        };
        assert_eq!(lock.path(), data.join(LOCK_FILE));
        let pid = std::fs::read_to_string(lock.path()).unwrap();
        assert_eq!(pid.trim(), std::process::id().to_string());

        assert!(CollectorLock::try_acquire(&data).unwrap().is_busy());
        drop(lock);
        assert!(!CollectorLock::try_acquire(&data).unwrap().is_busy());
    }

    #[test]
    fn daemon_lock_is_independent_of_run_lock() {
        let dir = tempfile::tempdir().unwrap();
        let LockAttempt::Acquired(daemon) = CollectorLock::try_acquire_daemon(dir.path()).unwrap()
        else {
            panic!("first daemon should acquire");
        };
        assert_eq!(daemon.path(), dir.path().join(DAEMON_LOCK_FILE));
        assert!(
            CollectorLock::try_acquire_daemon(dir.path())
                .unwrap()
                .is_busy()
        );
        assert!(!CollectorLock::try_acquire(dir.path()).unwrap().is_busy());
    }
}
