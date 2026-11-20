//! Process-wide exclusive lock for journal, baseline, and recovery mutations.
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

/// Holds the state lock until dropped. The kernel releases flock automatically
/// if a process exits or is killed, so stale lock files are harmless.
pub struct StateLock {
    file: File,
}

impl StateLock {
    pub fn acquire() -> Result<StateLock, String> {
        let home = crate::journal::home();
        fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
        let path: PathBuf = home.join("lifeline.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|e| format!("cannot open state lock {}: {e}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // Blocking exclusive lock: the kernel releases it on process exit.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result != 0 {
                return Err(format!(
                    "cannot acquire Lifeline state lock: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        #[cfg(not(unix))]
        return Err("Lifeline state locking is currently supported on Unix systems only".into());

        #[cfg(unix)]
        Ok(StateLock { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
