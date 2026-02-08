use futures::StreamExt;
use reqwest::Client;
use reqwest_eventsource::{Event, EventSource};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const PLEX_TV_API: &str = "https://plex.tv/api/v2";

const APP_NAME: &str = "discord-bot-for-plex";
const TMDB_API: &str = "https://api.themoviedb.org/3";
const TMDB_IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w500";
const DEFAULT_TMDB_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJhdWQiOiIzNmMxOTI3ZjllMTlkMzUxZWFmMjAxNGViN2JmYjNkZiIsIm5iZiI6MTc0NTQzMTA3NC4yMjcsInN1YiI6IjY4MDkyYTIyNmUxYTc2OWU4MWVmMGJhOSIsInNjb3BlcyI6WyJhcGlfcmVhZCJdLCJ2ZXJzaW9uIjoxfQ.Td6eAbW7SgQOMmQpRDwVM-_3KIMybGRqWNK8Yqw1Zzs";
const CACHE_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone)]
pub struct PlexConfig {
    pub server_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct IdentityResponse {
    pub media_container: IdentityContainer,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdentityContainer {
    #[serde(rename = "friendlyName")]
    pub friendly_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionsResponse {
    pub media_container: MediaContainer,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaContainer {
    #[serde(default)]
    pub metadata: Vec<SessionMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionMetadata {
    pub title: String,
    #[serde(rename = "type")]
    pub media_type: String,
    pub year: Option<u32>,
    pub duration: Option<u64>,
    #[serde(rename = "viewOffset")]
    pub view_offset: Option<u64>,
    #[serde(rename = "grandparentTitle")]
    pub grandparent_title: Option<String>,
    #[serde(rename = "parentTitle")]
    pub parent_title: Option<String>,
    #[serde(rename = "parentIndex")]
    pub parent_index: Option<u32>,
    pub index: Option<u32>,
    #[serde(rename = "User")]
    pub user: Option<PlexUser>,
    #[serde(rename = "Player")]
    pub player: Option<PlexPlayer>,
    #[serde(rename = "Guid", default)]
    pub guids: Vec<GuidTag>,
    pub key: Option<String>,
    #[serde(rename = "grandparentKey")]
    pub grandparent_key: Option<String>,
    #[serde(skip)]
    pub art_url: Option<String>,
    #[serde(skip)]
    pub server_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlexUser {
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlexPlayer {
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuidTag {
    pub id: String,
}

#[derive(Deserialize)]
struct TmdbImagesResponse {
    #[serde(default)]
    posters: Vec<TmdbImage>,
    #[serde(default)]
    backdrops: Vec<TmdbImage>,
}

#[derive(Deserialize)]
struct TmdbImage {
    file_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MetadataResponse {
    media_container: MetadataContainer,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MetadataContainer {
    #[serde(default)]
    metadata: Vec<ItemMetadata>,
}

#[derive(Deserialize)]
struct ItemMetadata {
    #[serde(rename = "Guid", default)]
    guids: Vec<GuidTag>,
}

struct CacheEntry {
    value: Option<String>,
    timestamp: Instant,
}

#[derive(Debug, Deserialize)]
pub struct PinResponse {
    pub id: u64,
    pub code: String,
    #[serde(rename = "authToken")]
    pub auth_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlexResource {
    pub name: String,
    #[serde(rename = "clientIdentifier")]
    pub client_identifier: String,
    #[serde(default)]
    pub connections: Vec<PlexConnection>,
    #[serde(rename = "accessToken")]
    pub access_token: Option<String>,
    pub provides: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlexConnection {
    pub uri: String,
    pub local: bool,
}

pub struct PlexAuth {
    client: Client,
}

impl PlexAuth {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent(APP_NAME)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    pub async fn request_pin(&self) -> Option<(u64, String)> {
        debug!("Requesting auth pin from Plex");
        let url = format!("{}/pins?strong=true", PLEX_TV_API);

        let resp = self
            .client
            .post(&url)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .header("X-Plex-Product", "Discord Bot for Plex")
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;

        let pin: PinResponse = resp.json().await.ok()?;
        debug!("Received pin ID: {}", pin.id);
        Some((pin.id, pin.code))
    }

    pub async fn check_pin(&self, pin_id: u64) -> Option<String> {
        debug!("Checking pin status for ID: {}", pin_id);
        let url = format!("{}/pins/{}", PLEX_TV_API, pin_id);

        let resp = self
            .client
            .get(&url)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;

        let pin: PinResponse = resp.json().await.ok()?;
        if pin.auth_token.is_some() {
            debug!("Pin authenticated successfully");
        }
        pin.auth_token
    }

    pub async fn get_servers(&self, token: &str) -> Vec<PlexResource> {
        debug!("Fetching Plex servers from API");
        let url = format!("{}/resources", PLEX_TV_API);

        let resp = match self
            .client
            .get(&url)
            .header("X-Plex-Token", token)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .header("Accept", "application/json")
            .query(&[("includeHttps", "1"), ("includeRelay", "1")])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                debug!("Failed to fetch servers: {}", e);
                return Vec::new();
            }
        };

        let servers: Vec<PlexResource> = resp
            .json::<Vec<PlexResource>>()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.provides.as_deref() == Some("server"))
            .collect();

        debug!("Found {} Plex server(s)", servers.len());
        servers
    }

    pub fn build_auth_url(&self, code: &str) -> String {
        format!(
            "https://app.plex.tv/auth#?clientID={}&code={}&context%5Bdevice%5D%5Bproduct%5D={}",
            urlencoding::encode(APP_NAME),
            urlencoding::encode(code),
            urlencoding::encode("Discord Bot for Plex")
        )
    }

    pub async fn get_server_urls(&self, token: &str, server_id: &str) -> Vec<String> {
        debug!("Getting URLs for server: {}", server_id);
        let servers = self.get_servers(token).await;
        let server = match servers.into_iter().find(|s| s.client_identifier == server_id) {
            Some(s) => s,
            None => {
                debug!("Server {} not found in resources", server_id);
                return Vec::new();
            }
        };

        let mut urls: Vec<String> = server
            .connections
            .iter()
            .filter(|c| !c.local)
            .map(|c| c.uri.trim_end_matches('/').to_string())
            .collect();

        let local_urls: Vec<String> = server
            .connections
            .iter()
            .filter(|c| c.local)
            .map(|c| c.uri.trim_end_matches('/').to_string())
            .collect();

        urls.extend(local_urls);
        debug!("Found {} URL(s) for server {}", urls.len(), server_id);
        urls
    }
}

impl SessionMetadata {
    pub fn progress_bar(&self) -> String {
        const BAR_WIDTH: usize = 10;

        let (offset, duration) = match (self.view_offset, self.duration) {
            (Some(o), Some(d)) if d > 0 => (o, d),
            _ => return format!("[{}] --%", "-".repeat(BAR_WIDTH)),
        };

        let progress = (offset as f64 / duration as f64).clamp(0.0, 1.0);
        let filled = (progress * BAR_WIDTH as f64) as usize;
        let empty = BAR_WIDTH - filled;
        let percent = (progress * 100.0) as u8;

        format!("[{}{}] {}%", "#".repeat(filled), "-".repeat(empty), percent)
    }
}

pub struct PlexClient {
    config: PlexConfig,
    auth: PlexAuth,
    active_url: Arc<RwLock<Option<String>>>,
    client: Client,
    sse_client: Client,
    sessions: Arc<RwLock<Vec<SessionMetadata>>>,
    server_name: Arc<RwLock<String>>,
    update_tx: broadcast::Sender<()>,
    tmdb_token: String,
    artwork_cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

impl PlexClient {
    pub fn new(config: PlexConfig) -> Self {
        debug!("Creating PlexClient for server: {}", config.server_id);
        let (update_tx, _) = broadcast::channel(16);

        let client = Client::builder()
            .user_agent(APP_NAME)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        let sse_client = Client::builder()
            .user_agent(APP_NAME)
            .build()
            .expect("Failed to build SSE client");

        let tmdb_token = std::env::var("TMDB_TOKEN")
            .unwrap_or_else(|_| DEFAULT_TMDB_TOKEN.to_string());

        Self {
            config,
            auth: PlexAuth::new(),
            active_url: Arc::new(RwLock::new(None)),
            client,
            sse_client,
            sessions: Arc::new(RwLock::new(Vec::new())),
            server_name: Arc::new(RwLock::new("Plex".to_string())),
            update_tx,
            tmdb_token,
            artwork_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_active_url(&self) -> Option<String> {
        self.active_url.read().await.clone()
    }

    async fn try_url(&self, url: &str) -> bool {
        debug!("Testing connection to: {}", url);
        let test_url = format!("{}/", url);
        match self
            .client
            .get(&test_url)
            .header("X-Plex-Token", &self.config.token)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .send()
            .await
        {
            Ok(resp) => {
                debug!("Connection to {} succeeded (status: {})", url, resp.status());
                true
            }
            Err(e) => {
                warn!("Connection error for {}: {}", url, e);
                false
            }
        }
    }

    pub async fn find_working_url(&self) -> Option<String> {
        debug!("Finding working URL for server: {}", self.config.server_id);

        if let Some(url) = self.get_active_url().await {
            debug!("Testing cached URL: {}", url);
            if self.try_url(&url).await {
                debug!("Cached URL still working");
                return Some(url);
            }
            debug!("Cached URL no longer working, searching for new URL");
        }

        let urls = self
            .auth
            .get_server_urls(&self.config.token, &self.config.server_id)
            .await;

        if urls.is_empty() {
            error!("No URLs found for server {}", self.config.server_id);
            return None;
        }

        debug!("Trying {} URL(s)", urls.len());
        for url in urls {
            info!("Trying Plex server at: {}", url);
            if self.try_url(&url).await {
                info!("Connected to Plex server at: {}", url);
                *self.active_url.write().await = Some(url.clone());
                return Some(url);
            }
            warn!("Failed to connect to: {}", url);
        }
        error!("No working Plex server URL found");
        None
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.update_tx.subscribe()
    }

    pub async fn trigger_update(&self) {
        debug!("Manual update triggered for server: {}", self.server_name.read().await);
        self.update_sessions().await;
    }

    pub async fn get_sessions(&self) -> Vec<SessionMetadata> {
        self.sessions.read().await.clone()
    }

    pub async fn server_name(&self) -> String {
        self.server_name.read().await.clone()
    }

    pub async fn fetch_server_identity(&self) {
        let base_url = match self.find_working_url().await {
            Some(url) => url,
            None => return,
        };

        let url = format!("{}/", base_url);
        match self
            .client
            .get(&url)
            .header("X-Plex-Token", &self.config.token)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(identity) = resp.json::<IdentityResponse>().await {
                    *self.server_name.write().await = identity.media_container.friendly_name;
                    info!("Connected to Plex server: {}", self.server_name.read().await);
                }
            }
            Err(e) => {
                warn!("Failed to fetch server identity: {}", e);
            }
        }
    }

    pub async fn fetch_sessions(&self) -> Result<Vec<SessionMetadata>, reqwest::Error> {
        let base_url = self.get_active_url().await.unwrap_or_default();
        let url = format!("{}/status/sessions", base_url);
        debug!("Fetching sessions from: {}", url);

        let response = self
            .client
            .get(&url)
            .header("X-Plex-Token", &self.config.token)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .header("Accept", "application/json")
            .send()
            .await?
            .json::<SessionsResponse>()
            .await?;

        debug!("Fetched {} session(s)", response.media_container.metadata.len());
        Ok(response.media_container.metadata)
    }

    async fn update_sessions(&self) {
        debug!("Updating sessions");
        match self.fetch_sessions().await {
            Ok(mut sessions) => {
                let server_name = self.server_name.read().await.clone();
                debug!("Processing {} session(s) for {}", sessions.len(), server_name);
                for session in &mut sessions {
                    session.server_name = server_name.clone();
                    self.enrich_artwork(session).await;
                }
                *self.sessions.write().await = sessions.clone();
                debug!("Broadcasting update notification");
                let _ = self.update_tx.send(());
            }
            Err(e) => {
                error!("Failed to fetch sessions: {}", e);
            }
        }
    }

    async fn enrich_artwork(&self, session: &mut SessionMetadata) {
        debug!("Enriching artwork for: {}", session.title);

        let tmdb_id = match self.get_tmdb_id(session).await {
            Some(id) => id,
            None => {
                debug!("No TMDB ID found for: {}", session.title);
                return;
            }
        };

        let media_path = match session.media_type.as_str() {
            "movie" => "movie",
            "episode" => "tv",
            _ => {
                debug!("Unsupported media type for artwork: {}", session.media_type);
                return;
            }
        };

        let cache_key = format!("{}:{}", media_path, tmdb_id);

        // Check cache
        {
            let cache = self.artwork_cache.read().await;
            if let Some(entry) = cache.get(&cache_key) {
                if entry.timestamp.elapsed().as_secs() < CACHE_TTL_SECS {
                    debug!("Using cached artwork for: {}", session.title);
                    session.art_url = entry.value.clone();
                    return;
                }
                debug!("Cache expired for: {}", cache_key);
            }
        }

        // Fetch from TMDB
        debug!("Fetching artwork from TMDB: {}/{}", media_path, tmdb_id);
        let art_url = self.fetch_tmdb_artwork(&tmdb_id, media_path).await;

        // Cache result
        {
            let mut cache = self.artwork_cache.write().await;
            cache.insert(
                cache_key,
                CacheEntry {
                    value: art_url.clone(),
                    timestamp: Instant::now(),
                },
            );
        }

        if let Some(ref url) = art_url {
            debug!("Got TMDB artwork: {}", url);
        } else {
            debug!("No artwork found for: {}", session.title);
        }
        session.art_url = art_url;
    }

    async fn get_tmdb_id(&self, session: &SessionMetadata) -> Option<String> {
        // First try to extract from session GUIDs
        for guid in &session.guids {
            if let Some(id) = guid.id.strip_prefix("tmdb://") {
                debug!("Found TMDB ID in session GUIDs: {}", id);
                return Some(id.to_string());
            }
        }

        // For episodes, fetch show metadata to get TMDB ID
        if session.media_type == "episode" {
            if let Some(ref gp_key) = session.grandparent_key {
                debug!("Fetching TMDB ID from show metadata: {}", gp_key);
                return self.fetch_tmdb_id_from_metadata(gp_key).await;
            }
        }

        // For movies, try fetching from item metadata
        if session.media_type == "movie" {
            if let Some(ref key) = session.key {
                debug!("Fetching TMDB ID from movie metadata: {}", key);
                return self.fetch_tmdb_id_from_metadata(key).await;
            }
        }

        debug!("No TMDB ID source available for: {}", session.title);
        None
    }

    async fn fetch_tmdb_id_from_metadata(&self, key: &str) -> Option<String> {
        let base_url = self.get_active_url().await?;
        let url = format!("{}{}", base_url, key);
        debug!("Fetching metadata from: {}", url);

        let resp = self
            .client
            .get(&url)
            .header("X-Plex-Token", &self.config.token)
            .header("X-Plex-Client-Identifier", APP_NAME)
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;

        let meta: MetadataResponse = resp.json().await.ok()?;
        let item = meta.media_container.metadata.first()?;

        for guid in &item.guids {
            if let Some(id) = guid.id.strip_prefix("tmdb://") {
                debug!("Found TMDB ID in metadata: {}", id);
                return Some(id.to_string());
            }
        }

        debug!("No TMDB ID found in metadata for: {}", key);
        None
    }

    async fn fetch_tmdb_artwork(&self, tmdb_id: &str, media_path: &str) -> Option<String> {
        let endpoint = format!("{}/{}/{}/images", TMDB_API, media_path, tmdb_id);
        debug!("Fetching TMDB images from: {}", endpoint);

        let resp: TmdbImagesResponse = self
            .client
            .get(&endpoint)
            .header("Authorization", format!("Bearer {}", self.tmdb_token))
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;

        debug!(
            "TMDB response: {} poster(s), {} backdrop(s)",
            resp.posters.len(),
            resp.backdrops.len()
        );

        resp.posters
            .first()
            .or(resp.backdrops.first())
            .map(|img| format!("{}{}", TMDB_IMAGE_BASE, img.file_path))
    }

    pub async fn start_sse_listener(self: Arc<Self>, cancel: CancellationToken) {
        info!("Connecting to Plex SSE endpoint");
        self.update_sessions().await;

        loop {
            if cancel.is_cancelled() {
                info!("SSE listener shutting down");
                break;
            }

            let base_url = match self.find_working_url().await {
                Some(url) => url,
                None => {
                    warn!("No working Plex URL, retrying in 10 seconds...");
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(10)) => continue,
                    }
                }
            };

            let sse_url = format!("{}/:/eventsource/notifications?filters=playing", base_url);
            debug!("Connecting SSE to: {}", sse_url);

            let request = self
                .sse_client
                .get(&sse_url)
                .header("Accept", "text/event-stream")
                .header("X-Plex-Token", &self.config.token)
                .header("X-Plex-Client-Identifier", APP_NAME);

            let mut es = match EventSource::new(request) {
                Ok(es) => {
                    debug!("EventSource created successfully");
                    es
                }
                Err(e) => {
                    error!("Failed to create EventSource: {:?}", e);
                    *self.active_url.write().await = None;
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    }
                }
            };

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        info!("SSE listener shutting down");
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(90)) => {
                        warn!("SSE connection timeout - no events received");
                        *self.active_url.write().await = None;
                        break;
                    }
                    event = es.next() => {
                        match event {
                            Some(Ok(Event::Open)) => {
                                info!("Connected to Plex SSE");
                            }
                            Some(Ok(Event::Message(msg))) => {
                                debug!("SSE event: {} - {}", msg.event, msg.data);
                                self.update_sessions().await;
                            }
                            Some(Err(e)) => {
                                warn!("SSE error: {:?}", e);
                                *self.active_url.write().await = None;
                                break;
                            }
                            None => {
                                debug!("SSE stream ended (None received)");
                                *self.active_url.write().await = None;
                                break;
                            }
                        }
                    }
                }
            }

            if cancel.is_cancelled() {
                break;
            }
            warn!("SSE connection closed, reconnecting in 5 seconds...");
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }
}
