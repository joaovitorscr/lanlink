//! Single-instance guard. Only one node may run per config dir: the app and the CLI share
//! one identity, and two endpoints with the same key fight over the relay and connections.
//!
//! `lanlink.lock` holds an OS file lock for the life of the node. The holder's pid goes in
//! `lanlink.pid` next to it (Windows locks block reading the locked file itself).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fs4::{FileExt, TryLockError};

/// Held while a node runs. Dropping it releases the lock.
#[derive(Debug)]
pub struct InstanceLock {
    file: File,
    pid_path: PathBuf,
}

impl InstanceLock {
    /// Take the lock in `dir`, or fail with a message naming the running process.
    pub fn acquire(dir: &Path) -> anyhow::Result<InstanceLock> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("lanlink.lock");
        let pid_path = dir.join("lanlink.pid");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        // Fully qualified: std's inherent `File::try_lock` would otherwise shadow fs4's.
        match FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let pid = std::fs::read_to_string(&pid_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                anyhow::bail!("{}", already_running(pid));
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", path.display()))
            }
        }
        if let Err(e) = std::fs::write(&pid_path, std::process::id().to_string()) {
            tracing::warn!("could not write {}: {e}", pid_path.display());
        }
        Ok(InstanceLock { file, pid_path })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Remove the pid first so a new holder's pid is never deleted.
        let _ = std::fs::remove_file(&self.pid_path);
        let _ = FileExt::unlock(&self.file);
    }
}

fn already_running(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => {
            format!("lanlink is already running (pid {pid}). Close it first, or use the app.")
        }
        None => "lanlink is already running. Close it first, or use the app.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanlink-instance-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn second_acquire_fails_until_released() {
        let dir = temp_dir("lock");
        let first = InstanceLock::acquire(&dir).unwrap();
        let err = InstanceLock::acquire(&dir).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "lanlink is already running (pid {}). Close it first, or use the app.",
                std::process::id()
            )
        );
        drop(first);
        let again = InstanceLock::acquire(&dir).unwrap();
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
