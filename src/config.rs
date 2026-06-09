//! Config file at `~/.config/mnml-msg-gmail/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives entirely in env (`GMAIL_CLIENT_ID`, `GMAIL_CLIENT_SECRET`)
//! and on-disk at `~/.config/mnml-msg-gmail/token.json` — never in
//! the TOML.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// Tab kind:
    ///   - `inbox` — messages with the INBOX label
    ///   - `sent` — messages with the SENT label
    ///   - `starred` — messages with the STARRED label
    ///   - `labels` — list of labels (Enter scopes to a label)
    ///   - `search` — interactive Gmail search query
    pub kind: String,
    /// `search`-only: initial query (Gmail search syntax). Optional.
    /// Other kinds ignore this.
    #[serde(default)]
    pub query: Option<String>,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-msg-gmail config. Edit and re-run.
#
# Auth lives in env vars (NOT here):
#   export GMAIL_CLIENT_ID=...        (from Google Cloud → OAuth credentials)
#   export GMAIL_CLIENT_SECRET=...    (paired with the above)
#
# Then `mnml-msg-gmail auth` to do the loopback consent flow once.
# The resulting token is persisted at ~/.config/mnml-msg-gmail/token.json.
#
# See the README for the Google Cloud project setup walkthrough.

refresh_interval_secs = 120

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "inbox"    — messages with the INBOX label
#   "sent"     — messages with the SENT label
#   "starred"  — messages with the STARRED label
#   "labels"   — list of labels (Enter scopes a transient view)
#   "search"   — interactive Gmail search

[[tabs]]
name = "inbox"
kind = "inbox"

[[tabs]]
name = "sent"
kind = "sent"

[[tabs]]
name = "starred"
kind = "starred"

[[tabs]]
name = "labels"
kind = "labels"

[[tabs]]
name = "search"
kind = "search"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "inbox" | "sent" | "starred" | "labels" | "search" => {}
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"inbox\", \"sent\", \"starred\", \"labels\", or \"search\")",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-msg-gmail")
        .join("config.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    let first_run = !path.exists();
    if first_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        eprintln!(
            "first run: wrote config template to {} — edit it to customize",
            path.display()
        );
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert_eq!(cfg.tabs.len(), 5);
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            refresh_interval_secs: 120,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 120,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
                query: None,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_all_known_kinds() {
        for kind in ["inbox", "sent", "starred", "labels", "search"] {
            let cfg = Config {
                refresh_interval_secs: 120,
                tabs: vec![Tab {
                    name: kind.into(),
                    kind: kind.into(),
                    query: None,
                }],
            };
            assert!(cfg.validate().is_ok(), "kind {kind:?} should validate");
        }
    }
}
