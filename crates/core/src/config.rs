use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "config.json";
// Lookout's own Entra app registration: a public client, so not a secret.
const CLIENT_ID: &str = "03441b94-3007-44a9-b3ac-7520f2e155ee";
// Each minute: Graph has no push to desktop apps, so new mail is polled.
const DEFAULT_POLL_SECONDS: u32 = 60;

/// Settings kept in `config.json` in the data folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The Entra application id; overrides the one built in.
    pub client_id: Option<String>,
    pub poll_seconds: u32,
    /// Loads pictures from the internet in messages without asking.
    pub load_remote_images: bool,
    pub dark: bool,
    /// Messages are shown as sent, in WebView2, rather than drawn natively.
    pub original_view: bool,
    /// Folders whose subfolders are hidden, and accounts whose folders are,
    /// as `account/folder` and `account/`.
    pub collapsed: Vec<String>,
    /// Folders listed first, as `account/folder`, in order.
    pub favourites: Vec<String>,
    /// Markdown added to new messages and replies, by account id.
    pub signatures: std::collections::HashMap<String, String>,
    /// Looks for a new version when Lookout starts and once a day.
    pub check_for_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: None,
            poll_seconds: DEFAULT_POLL_SECONDS,
            load_remote_images: false,
            dark: false,
            original_view: false,
            collapsed: Vec::new(),
            favourites: Vec::new(),
            signatures: std::collections::HashMap::new(),
            check_for_updates: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Ok(path) = data_dir().map(|dir| dir.join(CONFIG_FILE)) else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|error| {
                log::warn!("ignoring {}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> crate::Result<()> {
        let dir = data_dir()?;
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(dir.join(CONFIG_FILE), json)?;
        Ok(())
    }

    /// The application id to sign in with: the configured one, else one
    /// given with `LOOKOUT_CLIENT_ID` when building or running, else
    /// Lookout's own.
    pub fn client_id(&self) -> crate::Result<String> {
        Ok(self
            .client_id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .or_else(|| option_env!("LOOKOUT_CLIENT_ID").map(str::to_owned))
            .or_else(|| std::env::var("LOOKOUT_CLIENT_ID").ok())
            .unwrap_or_else(|| CLIENT_ID.to_owned()))
    }
}

/// The folder holding the cache, settings and the log. Local rather than
/// roaming: the cache is large and belongs to this computer.
pub fn data_dir() -> crate::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("LOOKOUT_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::data_local_dir()
        .ok_or(crate::Error::NoDataDir)?
        .join("lookout"))
}
