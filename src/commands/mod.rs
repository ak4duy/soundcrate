mod about;

mod library;
mod play;
mod queue;
mod shuffle;
mod voice;

pub(super) use about::About;
pub(super) use library::Library;
pub(super) use play::autoplay_next;

use anyhow::{Context as _, Result, bail};
use serenity::all::*;

use crate::Handler;

pub(super) fn definitions() -> Vec<CreateCommand> {
    let mut commands = vec![
        CreateCommand::new("about")
            .description("Show Soundcrate version, build details, and update status"),
        CreateCommand::new("playalbum")
            .description("Play or queue an album from audio metadata tags")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "album",
                    "Search album titles and album artists",
                )
                .required(true)
                .set_autocomplete(true),
            ),
        CreateCommand::new("playrandom")
            .description("Play or queue one random track from the library")
            .dm_permission(false),
        CreateCommand::new("autoplay")
            .description("Play random library tracks when the queue runs out")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "mode",
                    "Turn autoplay on or off",
                )
                .required(true)
                .add_string_choice("on", "on")
                .add_string_choice("off", "off"),
            ),
        CreateCommand::new("play")
            .description("Play or queue a track from the local library")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "track",
                    "Search your music library",
                )
                .required(true)
                .set_autocomplete(true),
            ),
        CreateCommand::new("playurl")
            .description("Play or queue audio from a direct HTTP(S) URL")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "url",
                    "Direct link to an audio file or stream",
                )
                .required(true),
            ),
        CreateCommand::new("shuffle")
            .description("Turn queue shuffle on or off")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "mode", "Shuffle mode")
                    .required(true)
                    .add_string_choice("all", "all")
                    .add_string_choice("off", "off"),
            ),
        CreateCommand::new("clear")
            .description("Clear track by index")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "track",
                    "Enter your queue index",
                )
                .required(true)
                .set_autocomplete(true),
            ),
    ];
    for (name, description) in [
        (
            "library",
            "Show library track counts, albums, file sizes, and formats",
        ),
        ("pause", "Pause playback"),
        ("resume", "Resume playback"),
        ("skip", "Skip the current track"),
        ("queue", "Show the current track and queue"),
        ("stop", "Clear the queue"),
    ] {
        commands.push(
            CreateCommand::new(name)
                .description(description)
                .dm_permission(false),
        );
    }
    commands
}

pub(super) async fn autocomplete(handler: &Handler, ctx: &Context, cmd: &CommandInteraction) {
    let query = cmd.data.autocomplete().map(|a| a.value).unwrap_or("");
    let mut response = CreateAutocompleteResponse::new();
    if cmd.data.name == "playalbum" {
        for (id, label) in handler.library.search_albums(query, 25) {
            response = response.add_string_choice(short(&label, 100), format!("album:{id}"));
        }
    } else if cmd.data.name == "clear" {
        if let Some(guild) = cmd.guild_id {
            for (label, value) in queue::clear_choices(handler, ctx, guild, query).await {
                response = response.add_string_choice(label, value);
            }
        }
    } else {
        for (id, track) in handler.library.search(query, 25) {
            response = response.add_string_choice(short(&track.label, 100), format!("track:{id}"));
        }
    }
    if let Err(error) = cmd
        .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        tracing::warn!(%error, "Autocomplete response failed");
    }
}

pub(super) async fn execute(
    handler: &Handler,
    ctx: &Context,
    cmd: &CommandInteraction,
) -> Result<(String, Vec<CreateActionRow>)> {
    let guild = cmd.guild_id.context("Use this command in a server.")?;
    let name = cmd.data.name.as_str();
    if name == "queue" {
        return queue_page(handler, ctx, guild, 0).await;
    }
    let query = cmd
        .data
        .options
        .iter()
        .find(|o| o.name == "track" || o.name == "album")
        .and_then(|o| o.value.as_str());

    let manager = songbird::get(ctx)
        .await
        .context("Voice service unavailable")?;
    let session = handler.session(guild).await;
    let mut session = session.lock().await;
    let user_channel = ctx
        .cache
        .guild(guild)
        .and_then(|g| g.voice_states.get(&cmd.user.id).and_then(|v| v.channel_id))
        .context("Join a voice channel first.")?;
    if let Some(call) = manager.get(guild) {
        let call = call.lock().await;
        if let Some(channel) = call.current_channel()
            && channel.0.get() != user_channel.get()
        {
            bail!("Join my voice channel to control playback.");
        }
    }
    let content = if matches!(
        name,
        "play" | "playalbum" | "playrandom" | "playurl" | "autoplay"
    ) {
        play::execute(
            handler,
            ctx,
            &manager,
            guild,
            user_channel,
            cmd,
            &mut session,
        )
        .await?
    } else {
        let call = manager
            .get(guild)
            .context("I’m not connected to a voice channel.")?;
        let call = call.lock().await;
        if name == "clear" {
            queue::clear_track(call.queue(), &mut session, query)?
        } else {
            let mode = cmd
                .data
                .options
                .iter()
                .find(|option| option.name == "mode")
                .and_then(|option| option.value.as_str());
            voice::execute(&call, &mut session, ctx, cmd.channel_id, name, mode).await?
        }
    };
    Ok((content, vec![]))
}

pub(super) async fn respond(handler: &Handler, ctx: &Context, cmd: &CommandInteraction) {
    if let Err(error) = cmd.defer(&ctx.http).await {
        tracing::warn!(%error, "Could not defer command");
        return;
    }
    let response = match cmd.data.name.as_str() {
        "about" => Some(about::response(&handler.about).await),
        "library" if cmd.guild_id.is_some() => Some(library::response(&handler.library)),
        _ => None,
    };
    if let Some(response) = response {
        if let Err(error) = cmd.edit_response(&ctx.http, response).await {
            tracing::warn!(%error, command = %cmd.data.name, "Command response failed");
        }
        return;
    }
    let (content, components) = match execute(handler, ctx, cmd).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, command = %cmd.data.name, "Command failed");
            (format!("{error}"), vec![])
        }
    };
    if let Err(error) = cmd
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new()
                .content("")
                .embed(CreateEmbed::new().description(content).color(0x4BFF9A))
                .components(components)
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await
    {
        tracing::warn!(%error, "Command response failed");
    }
}

pub(super) async fn component(handler: &Handler, ctx: &Context, component: &ComponentInteraction) {
    let Some(page) = component
        .data
        .custom_id
        .strip_prefix("queue:")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return;
    };
    let Some(guild) = component.guild_id else {
        return;
    };
    if let Err(error) = component
        .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
        .await
    {
        tracing::warn!(%error, "Could not defer queue page update");
        return;
    }
    let (content, components) = match queue_page(handler, ctx, guild, page).await {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(%error, "Queue page update failed");
            (format!("{error}"), vec![])
        }
    };
    if let Err(error) = component
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new()
                .content("")
                .embed(CreateEmbed::new().description(content).color(0x4BFF9A))
                .components(components)
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await
    {
        tracing::warn!(%error, "Could not edit queue page");
    }
}

pub(super) async fn queue_page(
    handler: &Handler,
    ctx: &Context,
    guild: GuildId,
    page: usize,
) -> Result<(String, Vec<CreateActionRow>)> {
    queue::page(handler, ctx, guild, page).await
}

pub(super) fn short(value: &str, max: usize) -> String {
    value
        .chars()
        .take(max)
        .map(|c| {
            if c.is_control() || matches!(c, '`' | '*' | '_' | '~' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect()
}
