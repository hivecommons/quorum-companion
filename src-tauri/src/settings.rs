//! Where the companion keeps its settings: the hive URL in a small JSON file in the app's config
//! directory, the companion token in the OS credential store (never in the file): the macOS
//! keychain, or Windows Credential Manager (generic credential, the `keyring` crate's
//! `windows-native` backend).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const KEYCHAIN_SERVICE: &str = "dev.hivecommons.quorum-companion";
const SETTINGS_FILE: &str = "settings.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub hive_url: String,
    /// Name the hive attributes the microphone channel to; empty = the hive user.
    #[serde(default)]
    pub host_name: String,
    /// Ask before every call (true) or never offer (false).
    #[serde(default = "default_true")]
    pub offer_on_call: bool,
}

fn default_true() -> bool {
    true
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE)
}

pub fn load(dir: &Path) -> Settings {
    fs::read_to_string(path(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Settings {
            offer_on_call: true,
            ..Settings::default()
        })
}

pub fn save(dir: &Path, s: &Settings) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    fs::write(path(dir), body).map_err(|e| e.to_string())
}

/// The keychain account a hive's token is stored under (one token per hive URL).
fn account(hive_url: &str) -> String {
    hive_url.trim().trim_end_matches('/').to_string()
}

pub fn token(hive_url: &str) -> Option<String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &account(hive_url))
        .ok()?
        .get_password()
        .ok()
        .filter(|t| !t.trim().is_empty())
}

pub fn set_token(hive_url: &str, token: &str) -> Result<(), String> {
    let entry =
        keyring::Entry::new(KEYCHAIN_SERVICE, &account(hive_url)).map_err(|e| e.to_string())?;
    if token.trim().is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    entry.set_password(token.trim()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default() {
        let dir = std::env::temp_dir().join(format!("qc-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(load(&dir).offer_on_call);
        let s = Settings {
            hive_url: "https://h.example".into(),
            host_name: "Ana".into(),
            offer_on_call: false,
        };
        save(&dir, &s).unwrap();
        assert_eq!(load(&dir), s);
        fs::write(path(&dir), "not json").unwrap();
        assert_eq!(load(&dir).hive_url, "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_accounts_ignore_a_trailing_slash() {
        assert_eq!(account(" https://h.example/ "), "https://h.example");
    }
}
