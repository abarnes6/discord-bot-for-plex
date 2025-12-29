use serde::{Deserialize, Serialize};
use std::env;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::error;

fn config_path() -> String {
    env::var("CONFIG_PATH").unwrap_or_else(|_| "config.json".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlexServer {
    pub server_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub plex_servers: Vec<PlexServer>,
    pub session_channel_id: Option<u64>,
    pub session_message_id: Option<u64>,
}

impl Config {
    pub async fn load() -> Self {
        let path = config_path();
        match fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub async fn save(&self) -> Result<(), std::io::Error> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(config_path(), content).await
    }
}

pub struct ConfigManager {
    config: RwLock<Config>,
}

impl ConfigManager {
    pub async fn new() -> Self {
        Self {
            config: RwLock::new(Config::load().await),
        }
    }

    pub async fn get(&self) -> Config {
        self.config.read().await.clone()
    }

    pub async fn set_session_channel(&self, channel_id: u64) {
        let mut config = self.config.write().await;
        config.session_channel_id = Some(channel_id);
        config.session_message_id = None;
        if let Err(e) = config.save().await {
            error!("Failed to save config: {}", e);
        }
    }

    pub async fn set_session_message(&self, message_id: u64) {
        let mut config = self.config.write().await;
        config.session_message_id = Some(message_id);
        if let Err(e) = config.save().await {
            error!("Failed to save config: {}", e);
        }
    }

    pub async fn clear_session(&self) {
        let mut config = self.config.write().await;
        config.session_channel_id = None;
        config.session_message_id = None;
        if let Err(e) = config.save().await {
            error!("Failed to save config: {}", e);
        }
    }

    pub async fn set_plex_servers(&self, servers: Vec<PlexServer>) {
        let mut config = self.config.write().await;
        config.plex_servers = servers;
        if let Err(e) = config.save().await {
            error!("Failed to save config: {}", e);
        }
    }

    pub async fn get_plex_servers(&self) -> Vec<PlexServer> {
        self.config.read().await.plex_servers.clone()
    }
}
