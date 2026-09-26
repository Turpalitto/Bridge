//! Persistent registry and configuration of synced folders.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::watcher::OutboxTarget;
use crate::CoreError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncFolder {
    pub path: PathBuf,
    pub target: OutboxTarget,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl SyncFolder {
    pub fn new(path: impl Into<PathBuf>, target: OutboxTarget) -> Self {
        Self {
            path: path.into(),
            target,
            enabled: true,
        }
    }
}

pub struct SyncRegistry {
    file_path: PathBuf,
}

impl SyncRegistry {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            file_path: state_dir.join("sync_folders.json"),
        }
    }

    pub fn load(&self) -> Vec<SyncFolder> {
        if !self.file_path.exists() {
            return Vec::new();
        }
        let data = match std::fs::read(&self.file_path) {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        serde_json::from_slice(&data).unwrap_or_default()
    }

    pub fn save(&self, folders: &[SyncFolder]) -> Result<(), CoreError> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(folders)
            .map_err(|e| CoreError::Io(std::io::Error::other(e)))?;
        std::fs::write(&self.file_path, data)?;
        Ok(())
    }

    pub fn add(&self, folder: SyncFolder) -> Result<(), CoreError> {
        let mut list = self.load();
        if let Some(existing) = list.iter_mut().find(|f| f.path == folder.path) {
            existing.target = folder.target;
            existing.enabled = folder.enabled;
        } else {
            list.push(folder);
        }
        self.save(&list)
    }

    pub fn remove(&self, path: &Path) -> Result<bool, CoreError> {
        let mut list = self.load();
        let len_before = list.len();
        list.retain(|f| f.path != path);
        if list.len() < len_before {
            self.save(&list)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_registry_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = SyncRegistry::new(tmp.path());
        assert_eq!(reg.load().len(), 0);

        let folder = SyncFolder::new("/tmp/test_sync", OutboxTarget::Auto);
        reg.add(folder.clone()).unwrap();

        let loaded = reg.load();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].path, PathBuf::from("/tmp/test_sync"));

        assert!(reg.remove(Path::new("/tmp/test_sync")).unwrap());
        assert_eq!(reg.load().len(), 0);
    }
}
