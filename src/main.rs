mod config;
mod discord;
mod embeds;
mod plex;

use config::{ConfigManager, PlexServer};
use dialoguer::{theme::ColorfulTheme, MultiSelect};
use discord::Handler;
use embeds::build_session_embeds;
use plex::{PlexAuth, PlexClient, PlexConfig};
use serenity::all::{ChannelId, EditMessage, Http, MessageId};
use serenity::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

async fn update_loop(
    http: Arc<Http>,
    plex_clients: Vec<Arc<PlexClient>>,
    config: Arc<ConfigManager>,
    cancel: CancellationToken,
) {
    use serenity::all::CreateMessage;
    use tokio::sync::broadcast;

    debug!("Starting update loop with {} Plex client(s)", plex_clients.len());
    let (aggregate_tx, mut aggregate_rx) = broadcast::channel::<()>(16);

    for (i, client) in plex_clients.iter().enumerate() {
        let mut rx = client.subscribe();
        let tx = aggregate_tx.clone();
        let cancel = cancel.clone();
        debug!("Spawning update forwarder for client {}", i);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!("Update forwarder {} shutting down", i);
                        break;
                    }
                    result = rx.recv() => {
                        match result {
                            Ok(()) => {
                                debug!("Forwarding update from client {}", i);
                                let _ = tx.send(());
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Update forwarder {} lagged by {} messages", i, n);
                                let _ = tx.send(());
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                debug!("Update forwarder {} channel closed", i);
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    debug!("Update loop ready, waiting for updates");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Update loop shutting down");
                break;
            }
            result = aggregate_rx.recv() => {
                match result {
                    Ok(()) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Update loop lagged by {} messages, continuing", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("Aggregate channel closed");
                        break;
                    }
                }

                debug!("Received update notification");
                let cfg = config.get().await;

                let channel_id = match cfg.session_channel_id {
                    Some(c) => ChannelId::new(c),
                    None => {
                        debug!("No session channel configured, skipping update");
                        continue;
                    }
                };

                let mut all_sessions = Vec::new();
                let mut server_names = Vec::new();
                for client in &plex_clients {
                    all_sessions.extend(client.get_sessions().await);
                    server_names.push(client.server_name().await);
                }

                debug!(
                    "Collected {} session(s) from {} server(s)",
                    all_sessions.len(),
                    server_names.len()
                );

                let embeds = build_session_embeds(&all_sessions, &server_names);
                debug!("Built {} embed(s)", embeds.len());

                let start = std::time::Instant::now();
                if let Some(msg_id) = cfg.session_message_id {
                    debug!("Updating existing message {}", msg_id);
                    let edit = EditMessage::new().embeds(embeds);
                    match channel_id
                        .edit_message(&http, MessageId::new(msg_id), edit)
                        .await
                    {
                        Ok(_) => {
                            debug!("Updated session board in {:?}", start.elapsed());
                        }
                        Err(e) => {
                            error!("Failed to update session board after {:?}: {}", start.elapsed(), e);
                        }
                    }
                } else {
                    debug!("Creating new session board message in channel {}", channel_id);
                    let msg = CreateMessage::new().embeds(embeds);
                    match channel_id.send_message(&http, msg).await {
                        Ok(message) => {
                            config.set_session_message(message.id.get()).await;
                            info!("Created new session board message (ID: {}) in {:?}", message.id, start.elapsed());
                        }
                        Err(e) => {
                            error!("Failed to create session message after {:?}: {}", start.elapsed(), e);
                        }
                    }
                }
            }
        }
    }
}

async fn run_auth_flow(auth: &PlexAuth) -> Option<Vec<PlexServer>> {
    debug!("Starting Plex auth flow");
    let (pin_id, code) = auth.request_pin().await?;
    let auth_url = auth.build_auth_url(&code);
    debug!("Auth URL generated for pin {}", pin_id);

    println!();
    println!("════════════════════════════════════════════════════════════");
    println!("  Plex Authentication Required");
    println!("════════════════════════════════════════════════════════════");
    println!();
    println!("  Open this link in your browser to sign in:");
    println!();
    println!("  {}", auth_url);
    println!();
    println!("  Waiting for authentication...");
    println!();

    let timeout = Duration::from_secs(300);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        tokio::time::sleep(Duration::from_secs(2)).await;

        if let Some(token) = auth.check_pin(pin_id).await {
            println!("  ✓ Authentication successful!");
            println!();

            let servers = auth.get_servers(&token).await;
            if servers.is_empty() {
                println!("  No Plex servers found on this account.");
                return None;
            }

            let server_names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();

            let selected_indices = MultiSelect::with_theme(&ColorfulTheme::default())
                .with_prompt("  Select servers to monitor (Enter to confirm)")
                .items(&server_names)
                .defaults(&vec![true; servers.len()])
                .interact()
                .ok()?;

            if selected_indices.is_empty() {
                println!("  No servers selected.");
                return None;
            }

            let selected_servers: Vec<PlexServer> = selected_indices
                .iter()
                .map(|&i| {
                    let server = &servers[i];
                    let server_token = server.access_token.clone().unwrap_or_else(|| token.clone());
                    PlexServer {
                        server_id: server.client_identifier.clone(),
                        token: server_token,
                    }
                })
                .collect();

            println!();
            println!("  ✓ Selected {} server(s):", selected_servers.len());
            for &i in &selected_indices {
                println!("    • {}", servers[i].name);
            }
            println!("════════════════════════════════════════════════════════════");
            println!();

            return Some(selected_servers);
        }
    }

    println!("  Authentication timed out.");
    None
}

fn servers_to_configs(servers: &[PlexServer]) -> Vec<PlexConfig> {
    servers
        .iter()
        .map(|s| PlexConfig {
            server_id: s.server_id.clone(),
            token: s.token.clone(),
        })
        .collect()
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    info!("Initializing Plex Discord Bot");
    debug!("Loading environment and config");

    let discord_token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set");
    debug!("Discord token loaded");

    let config = Arc::new(ConfigManager::new().await);
    debug!("Config manager initialized");

    let stored_servers = config.get_plex_servers().await;
    let plex_configs = if stored_servers.is_empty() {
        debug!("No stored servers, starting auth flow");
        let auth = PlexAuth::new();
        match run_auth_flow(&auth).await {
            Some(servers) => {
                debug!("Auth flow completed, saving {} server(s)", servers.len());
                config.set_plex_servers(servers.clone()).await;
                servers_to_configs(&servers)
            }
            None => {
                error!("Authentication failed or cancelled");
                return;
            }
        }
    } else {
        debug!("Found {} stored server(s)", stored_servers.len());
        servers_to_configs(&stored_servers)
    };

    debug!("Creating {} PlexClient(s)", plex_configs.len());
    let mut plex_clients: Vec<Arc<PlexClient>> = Vec::new();
    for (i, plex_config) in plex_configs.into_iter().enumerate() {
        debug!("Initializing Plex client {} (server: {})", i, plex_config.server_id);
        let client = Arc::new(PlexClient::new(plex_config));
        client.fetch_server_identity().await;
        plex_clients.push(client);
    }

    info!("Monitoring {} Plex server(s)", plex_clients.len());

    let handler = Handler {
        plex_clients: plex_clients.clone(),
        config: config.clone(),
    };

    let intents = GatewayIntents::GUILDS;
    debug!("Creating Discord client with GUILDS intent");

    let mut client = Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await
        .expect("Failed to create Discord client");

    let http = client.http.clone();
    let cancel = CancellationToken::new();

    info!("Starting Plex Discord Bot");

    debug!("Spawning {} SSE listener(s)", plex_clients.len());
    let mut sse_handles = Vec::new();
    for (i, plex_client) in plex_clients.iter().enumerate() {
        let plex_sse = plex_client.clone();
        let cancel_sse = cancel.clone();
        debug!("Spawning SSE listener {}", i);
        sse_handles.push(tokio::spawn(async move {
            plex_sse.start_sse_listener(cancel_sse).await;
        }));
    }

    debug!("Spawning update loop");
    let config_update = config.clone();
    let cancel_update = cancel.clone();
    let update_handle = tokio::spawn(async move {
        update_loop(http, plex_clients, config_update, cancel_update).await;
    });

    debug!("Starting Discord gateway connection");
    tokio::select! {
        result = client.start() => {
            if let Err(e) = result {
                error!("Discord client error: {:?}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal, stopping...");
        }
    }

    debug!("Cancelling background tasks");
    cancel.cancel();
    debug!("Waiting for SSE listeners to stop");
    for (i, handle) in sse_handles.into_iter().enumerate() {
        debug!("Waiting for SSE listener {}", i);
        let _ = handle.await;
    }
    debug!("Waiting for update loop to stop");
    let _ = update_handle.await;
    info!("Shutdown complete");
}
