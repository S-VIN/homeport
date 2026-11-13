//! Backups of the whole database (every plugin's settings and lists) in
//! `<data>/backups`. A copy is made automatically once a day (the last seven
//! automatic copies are kept) and before every restore; the user can make
//! one at any time. Restoring replaces the database and restarts the kernel.
use crate::{jobs::Task, Error, Kernel, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const AUTO_KEEP: usize = 7;
const DAY: u64 = 86_400;

#[derive(Clone, Debug, PartialEq)]
pub struct Backup {
    pub path: PathBuf,
    /// Unix seconds.
    pub created: u64,
    pub size: u64,
    /// Made by the daily schedule or before a restore (pruned automatically).
    pub auto: bool,
}

pub fn folder(data: &Path) -> PathBuf {
    data.join("backups")
}

/// `2026-11-13 14:05` (UTC) from Unix seconds.
pub fn stamp(secs: u64) -> String {
    let days = (secs / DAY) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let t = secs % DAY;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        t / 3600,
        t / 60 % 60
    )
}

pub fn list(data: &Path) -> Vec<Backup> {
    let mut out: Vec<Backup> = std::fs::read_dir(folder(data))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let rest = name.strip_prefix("rproxy-")?.strip_suffix(".sqlite3")?;
            let (created, auto) = match rest.split_once('-') {
                Some((t, _)) => (t.parse().ok()?, true),
                None => (rest.parse().ok()?, false),
            };
            Some(Backup {
                path: e.path(),
                created,
                size: e.metadata().map(|m| m.len()).unwrap_or(0),
                auto,
            })
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.created));
    out
}

impl Kernel {
    /// The backups, newest first (read from disk only after a change).
    pub fn backups(&self) -> Vec<Backup> {
        self.backup_list.lock().unwrap().clone()
    }
    fn refresh_backups(&self) {
        *self.backup_list.lock().unwrap() = list(&self.data_dir);
        self.repaint();
    }
    /// Copy the database now. `auto` copies are pruned to the last seven.
    pub fn back_up(&self, auto: bool) -> Result<Backup> {
        self.back_up_keeping(auto, None)
    }
    /// Like [`Kernel::back_up`], but pruning never removes `keep` (the copy being restored).
    pub(crate) fn back_up_keeping(&self, auto: bool, keep: Option<&Path>) -> Result<Backup> {
        let dir = folder(&self.data_dir);
        std::fs::create_dir_all(&dir).map_err(|e| Error::msg(format!("Cannot create {}: {e}", dir.display())))?;
        let mut now = crate::now();
        let name = |t: u64| format!("rproxy-{t}{}.sqlite3", if auto { "-auto" } else { "" });
        while dir.join(name(now)).exists() {
            now += 1;
        }
        let path = dir.join(name(now));
        self.store.backup_to(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let all = list(&self.data_dir);
        if auto {
            for old in all.iter().filter(|b| b.auto).skip(AUTO_KEEP).filter(|b| Some(b.path.as_path()) != keep) {
                let _ = std::fs::remove_file(&old.path);
            }
        }
        self.refresh_backups();
        Ok(all.into_iter().find(|b| b.path == path).expect("backup just written"))
    }
    /// Start the job "Back up settings" (a manual copy).
    pub fn start_backup(self: &Arc<Self>) -> Result<()> {
        let k = self.clone();
        let task = Task::new("kernel/backup-now", move |job| {
            let k = k.clone();
            async move {
                tokio::task::spawn_blocking(move || k.back_up(false)).await.map_err(|e| Error::msg(e.to_string()))??;
                job.summary("Backup saved");
                Ok(())
            }
        })
        .label("Back up settings");
        self.jobs.start(task).map(drop)
    }
    /// The daily copy, if the newest automatic one is older than a day.
    pub(crate) fn daily_backup(&self) {
        let newest = self.backups().into_iter().filter(|b| b.auto).map(|b| b.created).max();
        if newest.is_none_or(|t| crate::now().saturating_sub(t) >= DAY) {
            if let Err(e) = self.back_up(true) {
                self.notify_error("Couldn't back up settings", &e.message);
            }
        }
    }
    fn check(&self, path: &Path) -> Result<()> {
        if path.parent() != Some(folder(&self.data_dir).as_path()) || !path.is_file() {
            crate::bail!("Not a backup of this profile.");
        }
        Ok(())
    }
    pub fn delete_backup(&self, path: &Path) -> Result<()> {
        self.check(path)?;
        std::fs::remove_file(path).map_err(|e| Error::msg(e.to_string()))?;
        self.refresh_backups();
        Ok(())
    }
    /// Replace the database with `path` and restart the kernel (the current
    /// state is saved as an automatic backup first).
    pub fn restore_backup(&self, path: &Path) -> Result<()> {
        self.check(path)?;
        (self.restore)(path.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamps_are_utc_dates() {
        assert_eq!(super::stamp(0), "1970-01-01 00:00");
        assert_eq!(super::stamp(1_794_573_900), "2026-11-13 12:45");
    }
}
