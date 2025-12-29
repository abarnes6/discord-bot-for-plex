use crate::plex::SessionMetadata;
use serenity::builder::CreateEmbed;

pub fn build_session_embeds(sessions: &[SessionMetadata], server_names: &[String]) -> Vec<CreateEmbed> {
    if sessions.is_empty() {
        let footer_text = if server_names.len() == 1 {
            server_names[0].clone()
        } else {
            format!("{} servers", server_names.len())
        };
        return vec![CreateEmbed::new()
            .title("📺 Plex Activity")
            .description("No active sessions")
            .color(0x282a2d)
            .footer(serenity::builder::CreateEmbedFooter::new(footer_text))];
    }

    sessions.iter().map(build_session_embed).collect()
}

fn build_session_embed(session: &SessionMetadata) -> CreateEmbed {
    let user_name = session
        .user
        .as_ref()
        .map(|u| u.title.as_str())
        .unwrap_or("Unknown User");

    let player_state = session
        .player
        .as_ref()
        .map(|p| p.state.as_str())
        .unwrap_or("unknown");

    let description = match session.media_type.as_str() {
        "episode" => {
            let show = session.grandparent_title.as_deref().unwrap_or("Unknown Show");
            let season = session.parent_index.unwrap_or(0);
            let episode = session.index.unwrap_or(0);
            format!(
                "**{}**\n S{}·E{} - {}\n{}",
                show,
                season,
                episode,
                session.title,
                session.progress_bar()
            )
        }
        "movie" => {
            let year_str = session.year.map(|y| format!(" ({})", y)).unwrap_or_default();
            format!(
                "**{}**{}\n{}",
                session.title,
                year_str,
                session.progress_bar()
            )
        }
        "track" => {
            let artist = session.grandparent_title.as_deref().unwrap_or("Unknown Artist");
            let album = session.parent_title.as_deref().unwrap_or("Unknown Album");
            format!(
                "**{}** - {}\n{}\n{}",
                artist,
                session.title,
                album,
                session.progress_bar()
            )
        }
        _ => format!(
            "**{}**\n{}",
            session.title,
            session.progress_bar()
        ),
    };

    let mut embed = CreateEmbed::new()
        .title(format!("{} {}", user_name, player_state))
        .description(description)
        .color(match player_state {
            "playing" => 0xe5a00d,
            "paused" => 0x666666,
            _ => 0x282a2d,
        })
        .footer(serenity::builder::CreateEmbedFooter::new(&session.server_name));

    if let Some(ref art_url) = session.art_url {
        embed = embed.thumbnail(art_url);
    }

    embed
}
