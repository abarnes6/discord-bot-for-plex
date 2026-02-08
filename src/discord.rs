use crate::config::ConfigManager;
use crate::plex::PlexClient;
use serenity::all::{
    ChannelId, CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    CreateInteractionResponse, CreateInteractionResponseMessage, EventHandler, GuildId,
    Interaction, MessageId, Ready,
};
use serenity::async_trait;
use std::sync::Arc;
use tracing::{debug, error, info};

pub struct Handler {
    pub plex_clients: Vec<Arc<PlexClient>>,
    pub config: Arc<ConfigManager>,
}

impl Handler {
    async fn handle_set_channel(&self, command: &CommandInteraction) -> String {
        debug!("Handling plex-channel command");
        let channel_id = command
            .data
            .options
            .iter()
            .find(|opt| opt.name == "channel")
            .and_then(|opt| opt.value.as_channel_id());

        match channel_id {
            Some(id) => {
                debug!("Setting session channel to {}", id.get());
                self.config.set_session_channel(id.get()).await;
                self.trigger_all_updates().await;
                format!("Session board will now be displayed in <#{}>", id.get())
            }
            None => {
                debug!("No valid channel provided");
                "Please specify a valid channel".to_string()
            }
        }
    }

    async fn handle_refresh(&self) -> String {
        debug!("Handling plex-refresh command");
        self.trigger_all_updates().await;
        "Session board refreshed".to_string()
    }

    async fn trigger_all_updates(&self) {
        debug!("Triggering updates for {} Plex client(s)", self.plex_clients.len());
        for client in &self.plex_clients {
            client.trigger_update().await;
        }
    }

    async fn handle_clear(&self, ctx: &Context) -> String {
        debug!("Handling plex-clear command");
        let cfg = self.config.get().await;

        let (channel_id, message_id) = match (cfg.session_channel_id, cfg.session_message_id) {
            (Some(c), Some(m)) => (c, m),
            _ => {
                debug!("No session board message configured to clear");
                return "No session board message to clear".to_string();
            }
        };

        debug!("Deleting message {} from channel {}", message_id, channel_id);
        let channel = ChannelId::new(channel_id);
        let message = MessageId::new(message_id);

        match channel.delete_message(&ctx.http, message).await {
            Ok(_) => {
                debug!("Successfully deleted session board message");
                self.config.clear_session().await;
                "Session board cleared".to_string()
            }
            Err(e) => {
                error!("Failed to delete session board message: {}", e);
                self.config.clear_session().await;
                "Failed to delete message, but cleared config".to_string()
            }
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("Discord bot connected as {}", ready.user.name);
        debug!("Bot user ID: {}, guilds: {}", ready.user.id, ready.guilds.len());

        let commands = vec![
            CreateCommand::new("plex-channel")
                .description("Set the channel for the Plex session board")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Channel,
                        "channel",
                        "The channel to display sessions in",
                    )
                    .required(true),
                ),
            CreateCommand::new("plex-refresh")
                .description("Manually refresh the session board"),
            CreateCommand::new("plex-clear")
                .description("Remove the session board message"),
        ];

        for guild in &ready.guilds {
            debug!("Registering commands for guild {}", guild.id);
            if let Err(e) = GuildId::new(guild.id.get())
                .set_commands(&ctx.http, commands.clone())
                .await
            {
                error!("Failed to register commands for guild {}: {}", guild.id, e);
            }
        }

        info!("Slash commands registered for {} guild(s)", ready.guilds.len());
        self.trigger_all_updates().await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            debug!(
                "Received command: {} from user {} in guild {:?}",
                command.data.name,
                command.user.id,
                command.guild_id
            );

            let content = match command.data.name.as_str() {
                "plex-channel" => self.handle_set_channel(&command).await,
                "plex-refresh" => self.handle_refresh().await,
                "plex-clear" => self.handle_clear(&ctx).await,
                _ => {
                    debug!("Unknown command received: {}", command.data.name);
                    "Unknown command".to_string()
                }
            };

            let data = CreateInteractionResponseMessage::new()
                .content(content)
                .ephemeral(true);
            let builder = CreateInteractionResponse::Message(data);

            if let Err(e) = command.create_response(&ctx.http, builder).await {
                error!("Failed to respond to command: {}", e);
            } else {
                debug!("Successfully responded to command: {}", command.data.name);
            }
        }
    }
}
