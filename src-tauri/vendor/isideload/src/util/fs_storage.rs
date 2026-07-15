use std::path::{Path, PathBuf};

use rootcause::prelude::*;

use crate::util::storage::SideloadingStorage;

pub struct FsStorage {
    path: PathBuf,
}

impl FsStorage {
    pub fn new(path: PathBuf) -> Self {
        FsStorage { path }
    }
}

impl Default for FsStorage {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

impl SideloadingStorage for FsStorage {
    fn store_data(&self, key: &str, data: &[u8]) -> Result<(), Report> {
        let path = self.path.join(key);
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).context("Failed to create storage directory")?;
        std::fs::write(&path, data).context("Failed to write data to file")?;

        Ok(())
    }

    fn retrieve_data(&self, key: &str) -> Result<Option<Vec<u8>>, Report> {
        let path = self.path.join(key);
        match std::fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(report!(e).context("Failed to read data from file").into()),
        }
    }

    fn store(&self, key: &str, value: &str) -> Result<(), Report> {
        self.store_data(key, value.as_bytes())
    }

    fn retrieve(&self, key: &str) -> Result<Option<String>, Report> {
        match self.retrieve_data(key) {
            Ok(Some(data)) => Ok(Some(String::from_utf8_lossy(&data).into_owned())),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
