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
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

async fn update_loop(
    http: Arc<Http>,
    plex_clients: Vec<Arc<PlexClient>>,
    config: Arc<ConfigManager>,
    cancel: CancellationToken,
) {
    use serenity::all::CreateMessage;
    use tokio::sync::broadcast;

    let (aggregate_tx, mut aggregate_rx) = broadcast::channel::<()>(16);

    for client in &plex_clients {
        let mut rx = client.subscribe();
        let tx = aggregate_tx.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = rx.recv() => {
                        match result {
                            Ok(()) => {
                                let _ = tx.send(());
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                // Missed messages - just trigger an update anyway
                                let _ = tx.send(());
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Update loop shutting down");
                break;
            }
            result = aggregate_rx.recv() => {
                if result.is_err() {
                    break;
                }

                let cfg = config.get().await;

                let channel_id = match cfg.session_channel_id {
                    Some(c) => ChannelId::new(c),
                    None => continue,
                };

                let mut all_sessions = Vec::new();
                let mut server_names = Vec::new();
                for client in &plex_clients {
                    all_sessions.extend(client.get_sessions().await);
                    server_names.push(client.server_name().await);
                }

                let embeds = build_session_embeds(&all_sessions, &server_names);

                if let Some(msg_id) = cfg.session_message_id {
                    let edit = EditMessage::new().embeds(embeds);
                    if let Err(e) = channel_id
                        .edit_message(&http, MessageId::new(msg_id), edit)
                        .await
                    {
                        error!("Failed to update session board: {}", e);
                    }
                } else {
                    let msg = CreateMessage::new().embeds(embeds);
                    match channel_id.send_message(&http, msg).await {
                        Ok(message) => {
                            config.set_session_message(message.id.get()).await;
                            info!("Created new session board message");
                        }
                        Err(e) => {
                            error!("Failed to create session message: {}", e);
                        }
                    }
                }
            }
        }
    }
}

async fn run_auth_flow(auth: &PlexAuth) -> Option<Vec<PlexServer>> {
    let (pin_id, code) = auth.request_pin().await?;
    let auth_url = auth.build_auth_url(&code);

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

    let discord_token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set");
    let config = Arc::new(ConfigManager::new().await);

    let stored_servers = config.get_plex_servers().await;
    let plex_configs = if stored_servers.is_empty() {
        let auth = PlexAuth::new();
        match run_auth_flow(&auth).await {
            Some(servers) => {
                config.set_plex_servers(servers.clone()).await;
                servers_to_configs(&servers)
            }
            None => {
                error!("Authentication failed or cancelled");
                return;
            }
        }
    } else {
        servers_to_configs(&stored_servers)
    };

    let mut plex_clients: Vec<Arc<PlexClient>> = Vec::new();
    for plex_config in plex_configs {
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

    let mut client = Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await
        .expect("Failed to create Discord client");

    let http = client.http.clone();
    let cancel = CancellationToken::new();

    info!("Starting Plex Discord Bot");

    let mut sse_handles = Vec::new();
    for plex_client in &plex_clients {
        let plex_sse = plex_client.clone();
        let cancel_sse = cancel.clone();
        sse_handles.push(tokio::spawn(async move {
            plex_sse.start_sse_listener(cancel_sse).await;
        }));
    }

    let config_update = config.clone();
    let cancel_update = cancel.clone();
    let update_handle = tokio::spawn(async move {
        update_loop(http, plex_clients, config_update, cancel_update).await;
    });

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

    cancel.cancel();
    for handle in sse_handles {
        let _ = handle.await;
    }
    let _ = update_handle.await;
    info!("Shutdown complete");
}
