use crate::util::storage::SideloadingStorage;
use keyring::Entry;
use rootcause::prelude::*;

pub struct KeyringStorage {
    pub service_name: String,
}

impl KeyringStorage {
    pub fn new(service_name: String) -> Self {
        KeyringStorage { service_name }
    }
}

impl Default for KeyringStorage {
    fn default() -> Self {
        KeyringStorage {
            service_name: "isideload".to_string(),
        }
    }
}

impl SideloadingStorage for KeyringStorage {
    fn store(&self, key: &str, value: &str) -> Result<(), Report> {
        Entry::new(&self.service_name, key)?.set_password(value)?;
        Ok(())
    }

    fn retrieve(&self, key: &str) -> Result<Option<String>, Report> {
        let entry = Entry::new(&self.service_name, key)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn delete(&self, key: &str) -> Result<(), Report> {
        let entry = Entry::new(&self.service_name, key)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // Linux doesn't seem to properly retrive binary secrets, so we don't use this implementation and instead let it fall back to base64 encoding.
    // Windows fails to store the base64 encoded data because it is too long.
    #[cfg(target_os = "windows")]
    fn store_data(&self, key: &str, value: &[u8]) -> Result<(), Report> {
        Entry::new(&self.service_name, key)?.set_secret(value)?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn retrieve_data(&self, key: &str) -> Result<Option<Vec<u8>>, Report> {
        let entry = Entry::new(&self.service_name, key)?;
        match entry.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
