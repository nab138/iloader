use plist::Dictionary;
use plist_macro::pretty_print_dictionary;
use rootcause::prelude::*;
use serde::de::DeserializeOwned;
use tracing::error;

pub struct SensitivePlistAttachment {
    pub plist: Dictionary,
}

impl SensitivePlistAttachment {
    pub fn new(plist: Dictionary) -> Self {
        SensitivePlistAttachment { plist }
    }

    pub fn from_text(text: &str) -> Self {
        let dict: Result<Dictionary, _> = plist::from_bytes(text.as_bytes());
        match dict {
            Err(e) => {
                error!(
                    "Failed to parse plist text for sensitive attachment, returning empty plist: {:?}",
                    e
                );
                return SensitivePlistAttachment::new(Dictionary::new());
            }
            Ok(d) => SensitivePlistAttachment::new(d),
        }
    }

    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // if env variable DEBUG_SENSITIVE is set, print full plist
        if std::env::var("DEBUG_SENSITIVE").is_ok() {
            return writeln!(f, "{}", pretty_print_dictionary(&self.plist));
        }
        writeln!(
            f,
            "<Potentially sensitive data - set DEBUG_SENSITIVE env variable to see contents>"
        )
    }
}

impl std::fmt::Display for SensitivePlistAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.fmt(f)
    }
}

impl std::fmt::Debug for SensitivePlistAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.fmt(f)
    }
}

pub trait PlistDataExtract {
    fn get_data(&self, key: &str) -> Result<&[u8], Report>;
    fn get_str(&self, key: &str) -> Result<&str, Report>;
    fn get_string(&self, key: &str) -> Result<String, Report>;
    fn get_signed_integer(&self, key: &str) -> Result<i64, Report>;
    fn get_dict(&self, key: &str) -> Result<&Dictionary, Report>;
    fn get_bool(&self, key: &str) -> Result<bool, Report>;
    fn get_struct<T: DeserializeOwned>(&self, key: &str) -> Result<T, Report>;
}

impl PlistDataExtract for Dictionary {
    fn get_data(&self, key: &str) -> Result<&[u8], Report> {
        self.get(key).and_then(|v| v.as_data()).ok_or_else(|| {
            report!("Plist missing data for key '{}'", key)
                .attach(SensitivePlistAttachment::new(self.clone()))
        })
    }

    fn get_str(&self, key: &str) -> Result<&str, Report> {
        self.get(key).and_then(|v| v.as_string()).ok_or_else(|| {
            report!("Plist missing string for key '{}'", key)
                .attach(SensitivePlistAttachment::new(self.clone()))
        })
    }

    fn get_string(&self, key: &str) -> Result<String, Report> {
        self.get(key)
            .and_then(|v| v.as_string())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                report!("Plist missing string for key '{}'", key)
                    .attach(SensitivePlistAttachment::new(self.clone()))
            })
    }

    fn get_signed_integer(&self, key: &str) -> Result<i64, Report> {
        self.get(key)
            .and_then(|v| v.as_signed_integer())
            .ok_or_else(|| {
                report!("Plist missing signed integer for key '{}'", key)
                    .attach(SensitivePlistAttachment::new(self.clone()))
            })
    }

    fn get_dict(&self, key: &str) -> Result<&Dictionary, Report> {
        self.get(key)
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| {
                report!("Plist missing dictionary for key '{}'", key)
                    .attach(SensitivePlistAttachment::new(self.clone()))
            })
    }

    fn get_struct<T: DeserializeOwned>(&self, key: &str) -> Result<T, Report> {
        let dict = self.get(key);
        let dict = dict.ok_or_else(|| {
            report!("Plist missing dictionary for key '{}'", key)
                .attach(SensitivePlistAttachment::new(self.clone()))
        })?;
        let struct_data: T = plist::from_value(dict).map_err(|e| {
            report!(
                "Failed to deserialize plist struct for key '{}': {:?}",
                key,
                e
            )
            .attach(SensitivePlistAttachment::new(
                dict.as_dictionary().cloned().unwrap_or_default(),
            ))
        })?;
        Ok(struct_data)
    }

    fn get_bool(&self, key: &str) -> Result<bool, Report> {
        self.get(key).and_then(|v| v.as_boolean()).ok_or_else(|| {
            report!("Plist missing boolean for key '{}'", key)
                .attach(SensitivePlistAttachment::new(self.clone()))
        })
    }
}
