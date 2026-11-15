//! Non-secret settings (config.toml) and the API token (OS keyring).

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "rapido";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// JMAP session URL, e.g. Fastmail's.
    pub session_url: String,
    /// Username from the session, shown in the UI and used as the keyring key.
    pub username: String,
    /// JMAP account id of the mail account.
    #[serde(default)]
    pub account_id: String,
}

fn dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("cl", "kddlb", "rapido").context("no home directory")
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        Ok(dirs()?.config_dir().join("config.toml"))
    }

    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(
                toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    fn keyring_user(&self) -> String {
        format!("{} {}", self.username, self.session_url)
    }

    pub fn token(&self) -> Result<String> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_user())?;
        Ok(entry.get_password()?)
    }

    pub fn store_token(&self, token: &str) -> Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_user())?;
        entry.set_password(token)?;
        Ok(())
    }

    pub fn forget(&self) -> Result<()> {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_user()) {
            let _ = entry.delete_credential();
        }
        let _ = std::fs::remove_file(Self::path()?);
        Ok(())
    }

    /// Where the cache database for an account lives.
    pub fn database_path(account: &str) -> Result<PathBuf> {
        let dir = dirs()?.data_dir().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let safe: String = account
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Ok(dir.join(format!("{safe}.sqlite")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_round_trip() {
        let c = Config {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            username: "me@example.com".into(),
            account_id: "u123".into(),
        };
        let s = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<Config>(&s).unwrap(), c);
    }
}
