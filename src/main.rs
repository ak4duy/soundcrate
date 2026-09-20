mod library;

use std::{
    collections::HashMap,
    env,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use library::Library;
use serenity::{all::*, async_trait};
use songbird::{SerenityInit, input::File};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[derive(Default)]
struct Session {
    titles: HashMap<String, String>,
    idle_since: Option<Instant>,
}

struct Handler {
    library: Arc<Library>,
    sessions: Arc<Mutex<HashMap<GuildId, Arc<Mutex<Session>>>>>,
    guild_id: Option<GuildId>,
}

impl Handler {
    async fn session(&self, guild: GuildId) -> Arc<Mutex<Session>> {
        self.sessions.lock().await.entry(guild).or_default().clone()
    }

    async fn queue_page(
        &self,
        ctx: &Context,
        guild: GuildId,
        page: usize,
    ) -> Result<(String, Vec<CreateActionRow>)> {
        let manager = songbird::get(ctx)
            .await
            .context("Voice service unavailable")?;
        let session = self.session(guild).await;
        let session = session.lock().await;
        let Some(call) = manager.get(guild) else {
            return Ok(render_queue_page(&[], page));
        };
        let call = call.lock().await;
        let titles: Vec<_> = call
            .queue()
            .current_queue()
            .iter()
            .map(|track| {
                session
                    .titles
                    .get(&track.uuid().to_string())
                    .cloned()
                    .unwrap_or_else(|| "Unknown track".into())
            })
            .collect();
        Ok(render_queue_page(&titles, page))
    }

    async fn execute(
        &self,
        ctx: &Context,
        guild: GuildId,
        user: UserId,
        channel: ChannelId,
        name: &str,
        query: Option<&str>,
    ) -> Result<String> {
        let manager = songbird::get(ctx)
            .await
            .context("Voice service unavailable")?;
        let session = self.session(guild).await;
        let mut session = session.lock().await;

        let user_channel = ctx
            .cache
            .guild(guild)
            .and_then(|g| g.voice_states.get(&user).and_then(|v| v.channel_id))
            .context("Join a voice channel first.")?;
        if let Some(call) = manager.get(guild) {
            let call = call.lock().await;
            if let Some(channel) = call.current_channel()
                && channel.0.get() != user_channel.get()
            {
                bail!("Join my voice channel to control playback.");
            }
        }
        if matches!(name, "play" | "playalbum" | "playrandom") {
            let (label, tracks) = match name {
                "playalbum" => self
                    .library
                    .resolve_album(query.context("Choose an album.")?)?,
                "playrandom" => {
                    let track = self.library.random_track()?;
                    (track.label.clone(), vec![track])
                }
                _ => {
                    let track = self.library.resolve(query.context("Choose a track.")?)?;
                    (track.label.clone(), vec![track])
                }
            };
            let queued = if let Some(call) = manager.get(guild) {
                call.lock().await.queue().len()
            } else {
                0
            };
            check_queue_capacity(queued, tracks.len())?;
            let mut prepared = Vec::with_capacity(tracks.len());
            for track in tracks {
                let canonical = tokio::fs::canonicalize(&track.path)
                    .await
                    .with_context(|| format!("File is no longer available: {}", track.label))?;
                if !canonical.starts_with(&self.library.root) {
                    bail!("Track is outside the music directory.");
                }
                tokio::fs::File::open(&canonical)
                    .await
                    .with_context(|| format!("Cannot read track: {}", track.label))?;
                prepared.push((track, canonical));
            }
            let call = manager
                .join(guild, user_channel)
                .await
                .context("Could not join voice; check Connect and Speak permissions.")?;
            let mut call = call.lock().await;
            call.deafen(true).await?;
            let count = prepared.len();
            check_queue_capacity(call.queue().len(), count)?;
            let position = call.queue().len() + 1;
            for (track, canonical) in prepared {
                let mut audio = songbird::tracks::Track::from(File::new(canonical));
                audio.events.add_event(
                    songbird::events::EventData::new(
                        songbird::Event::Track(songbird::TrackEvent::Error),
                        PlaybackError {
                            http: ctx.http.clone(),
                            channel,
                            title: short(&track.label, 150),
                        },
                    ),
                    Duration::ZERO,
                );
                let handle = call.enqueue(audio).await;
                session
                    .titles
                    .insert(handle.uuid().to_string(), track.label.clone());
            }
            session.idle_since = None;
            if name == "playalbum" {
                return Ok(format!(
                    "{} album **{}** — {count} tracks ({position}).",
                    if position == 1 { "Playing" } else { "Queued" },
                    short(&label, 150)
                ));
            }
            return Ok(format!(
                "{} **{}** ({}).",
                if position == 1 { "Playing" } else { "Queued" },
                short(&label, 150),
                position,
            ));
        }
        let call = manager
            .get(guild)
            .context("I’m not connected to a voice channel.")?;
        let call = call.lock().await;
        match name {
            "pause" => {
                call.queue()
                    .current()
                    .context("Nothing is playing.")?
                    .pause()?;
                Ok("Paused.".into())
            }
            "resume" => {
                call.queue()
                    .current()
                    .context("The queue is empty.")?
                    .play()?;
                Ok("Resumed.".into())
            }
            "skip" => {
                if call.queue().is_empty() {
                    bail!("The queue is empty.");
                }
                call.queue().skip()?;
                Ok("Skipped.".into())
            }
            "stop" => {
                call.queue().stop();
                session.titles.clear();
                session.idle_since = None;
                Ok("Queue cleared.".into())
            }
            _ => bail!("Unknown command."),
        }
    }
}

struct PlaybackError {
    http: Arc<Http>,
    channel: ChannelId,
    title: String,
}

#[async_trait]
impl songbird::EventHandler for PlaybackError {
    async fn act(&self, ctx: &songbird::EventContext<'_>) -> Option<songbird::Event> {
        error!(?ctx, "Audio playback failed");
        let message = CreateMessage::new()
            .content(format!(
                "Could not play **{}**. Check that the file is valid and uses a supported codec.",
                self.title
            ))
            .allowed_mentions(CreateAllowedMentions::new());
        if let Err(error) = self.channel.send_message(&self.http, message).await {
            warn!(%error, "Could not report playback error");
        }
        None
    }
}

fn check_queue_capacity(queued: usize, added: usize) -> Result<()> {
    let remaining = 100_usize.saturating_sub(queued);
    if added > remaining {
        bail!("Queue is full. Nothing was added.");
    }
    Ok(())
}

fn short(value: &str, max: usize) -> String {
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

fn render_queue_page(titles: &[String], requested_page: usize) -> (String, Vec<CreateActionRow>) {
    if titles.is_empty() {
        return ("The queue is empty.".into(), vec![]);
    }
    let pages = titles.len().div_ceil(10);
    let page = requested_page.min(pages - 1);
    let mut lines = vec![format!("**Queue — {} tracks**", titles.len())];
    for (index, title) in titles.iter().enumerate().skip(page * 10).take(10) {
        lines.push(format!(
            "{}. {}{}",
            index + 1,
            short(title, 130),
            if index == 0 { " (current)" } else { "" },
        ));
    }
    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(format!("queue:{}", page.saturating_sub(1)))
            .label("◄")
            .style(ButtonStyle::Secondary)
            .disabled(page == 0),
        CreateButton::new("queue:page")
            .label(format!("{} / {pages}", page + 1))
            .style(ButtonStyle::Secondary)
            .disabled(true),
        CreateButton::new(format!("queue:{}", page + 1))
            .label("►")
            .style(ButtonStyle::Secondary)
            .disabled(page + 1 == pages),
    ]);
    (lines.join("\n"), vec![buttons])
}

fn commands() -> Vec<CreateCommand> {
    let mut commands = vec![
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
    ];
    for (name, description) in [
        ("pause", "Pause playback"),
        ("resume", "Resume playback"),
        ("skip", "Skip the current track"),
        ("queue", "Show the current track and queue"),
        ("stop", "Clear the queue and disconnect"),
    ] {
        commands.push(
            CreateCommand::new(name)
                .description(description)
                .dm_permission(false),
        );
    }
    commands
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let result = match self.guild_id {
            Some(guild) => guild.set_commands(&ctx.http, commands()).await,
            None => Command::set_global_commands(&ctx.http, commands()).await,
        };
        match result {
            Ok(_) => {
                info!(user = %ready.user.name, tracks = self.library.tracks.len(), "Soundcrate is ready")
            }
            Err(error) => error!(%error, "Could not register slash commands"),
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Autocomplete(cmd) => {
                let query = cmd.data.autocomplete().map(|a| a.value).unwrap_or("");
                let mut response = CreateAutocompleteResponse::new();
                if cmd.data.name == "playalbum" {
                    for (id, label) in self.library.search_albums(query, 25) {
                        response =
                            response.add_string_choice(short(&label, 100), format!("album:{id}"));
                    }
                } else {
                    for (id, track) in self.library.search(query, 25) {
                        response = response
                            .add_string_choice(short(&track.label, 100), format!("track:{id}"));
                    }
                }
                if let Err(error) = cmd
                    .create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
                    .await
                {
                    warn!(%error, "Autocomplete response failed");
                }
            }
            Interaction::Component(component) => {
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
                    warn!(%error, "Could not defer queue page update");
                    return;
                }
                let (content, components) = match self.queue_page(&ctx, guild, page).await {
                    Ok(page) => page,
                    Err(error) => {
                        warn!(%error, "Queue page update failed");
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
                    warn!(%error, "Could not edit queue page");
                }
            }
            Interaction::Command(cmd) => {
                if let Err(error) = cmd.defer(&ctx.http).await {
                    warn!(%error, "Could not defer command");
                    return;
                }
                let result = if let Some(guild) = cmd.guild_id {
                    if cmd.data.name == "queue" {
                        self.queue_page(&ctx, guild, 0).await
                    } else {
                        let query = cmd
                            .data
                            .options
                            .iter()
                            .find(|o| o.name == "track" || o.name == "album")
                            .and_then(|o| o.value.as_str());
                        self.execute(
                            &ctx,
                            guild,
                            cmd.user.id,
                            cmd.channel_id,
                            &cmd.data.name,
                            query,
                        )
                        .await
                        .map(|content| (content, vec![]))
                    }
                } else {
                    Err(anyhow::anyhow!("Use this command in a server."))
                };
                let (content, components) = match result {
                    Ok(response) => response,
                    Err(error) => {
                        warn!(%error, command = %cmd.data.name, "Command failed");
                        (format!("{error}"), vec![])
                    }
                };
                // embedded message
                if let Err(error) = cmd
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content("")
                            .embed(
                                CreateEmbed::new().description(content).color(0x4BFF9A), // kinda greeny
                            )
                            .components(components)
                            .allowed_mentions(CreateAllowedMentions::new()),
                    )
                    .await
                {
                    warn!(%error, "Command response failed");
                }
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "soundcrate=info,songbird=warn,serenity=warn".into()),
        )
        .init();
    let music_dir = env::var("MUSIC_DIR").unwrap_or_else(|_| "/music".into());
    let library = Arc::new(Library::scan(Path::new(&music_dir))?);
    info!(tracks = library.tracks.len(), "Indexed music library");
    if env::args().any(|arg| arg == "--check-library") {
        for track in &library.tracks {
            println!("{}", track.label);
        }
        return Ok(());
    }
    if library.tracks.is_empty() {
        warn!("No supported music files found in MUSIC_DIR");
    }
    let token = env::var("DISCORD_TOKEN").context("Set DISCORD_TOKEN")?;
    if token.trim().is_empty() {
        bail!("DISCORD_TOKEN must not be empty");
    }
    let guild_id = env::var("GUILD_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u64>())
        .transpose()
        .context("GUILD_ID must be a positive integer")?;
    if guild_id == Some(0) {
        bail!("GUILD_ID must be nonzero");
    }
    let idle_seconds: u64 = env::var("IDLE_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "300".into())
        .parse()
        .context("IDLE_TIMEOUT_SECONDS must be an integer")?;
    if idle_seconds == 0 {
        bail!("IDLE_TIMEOUT_SECONDS must be greater than zero");
    }
    let sessions = Arc::new(Mutex::new(HashMap::new()));
    let handler = Handler {
        library,
        sessions: sessions.clone(),
        guild_id: guild_id.map(GuildId::new),
    };
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let manager = songbird::Songbird::serenity();
    let mut client = Client::builder(token, intents)
        .event_handler(handler)
        .register_songbird_with(manager.clone())
        .await?;
    let idle_manager = manager.clone();
    let idle_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let entries: Vec<_> = sessions
                .lock()
                .await
                .iter()
                .map(|(id, session)| (*id, session.clone()))
                .collect();
            for (guild, session) in entries {
                let mut session = session.lock().await;
                let Some(call) = idle_manager.get(guild) else {
                    continue;
                };
                let mut call = call.lock().await;
                let queue = call.queue().current_queue();
                session
                    .titles
                    .retain(|id, _| queue.iter().any(|track| track.uuid().to_string() == *id));
                if call.current_channel().is_none() || !queue.is_empty() {
                    session.idle_since = None;
                    continue;
                }
                let since = session.idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(idle_seconds) {
                    if let Err(error) = call.leave().await {
                        warn!(%error, "Idle disconnect failed");
                    }
                    session.idle_since = None;
                }
            }
        }
    });
    let result = tokio::select! {
        result = client.start() => result.context("Discord client stopped"),
        _ = shutdown_signal() => { info!("Shutting down"); Ok(()) },
    };
    idle_task.abort();
    let guilds: Vec<_> = manager.iter().map(|(guild, _)| guild).collect();
    for guild in guilds {
        let _ = manager.remove(guild).await;
    }
    client.shard_manager.shutdown_all().await;
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_pages_show_ten_tracks_and_correct_buttons() {
        let titles: Vec<_> = (1..=21).map(|n| format!("Track {n}")).collect();
        for (page, start, end, previous_disabled, next_disabled) in [
            (0, 1, 10, true, false),
            (1, 11, 20, false, false),
            (2, 21, 21, false, true),
            (usize::MAX, 21, 21, false, true),
        ] {
            let (content, rows) = render_queue_page(&titles, page);
            let lines: Vec<_> = content.lines().skip(1).collect();
            assert_eq!(lines.len(), end - start + 1);
            assert!(lines[0].starts_with(&format!("{start}. Track {start}")));
            assert!(
                lines
                    .last()
                    .unwrap()
                    .starts_with(&format!("{end}. Track {end}"))
            );
            assert_eq!(content.contains("(current)"), page == 0);
            let json = serenity::json::to_value(&rows).unwrap();
            let buttons = &json[0]["components"];
            assert_eq!(buttons[0]["disabled"], previous_disabled);
            assert_eq!(buttons[1]["disabled"], true);
            assert_eq!(buttons[1]["label"], format!("{} / 3", page.min(2) + 1));
            assert_eq!(buttons[2]["disabled"], next_disabled);
            assert_eq!(
                buttons[0]["custom_id"],
                format!("queue:{}", page.min(2).saturating_sub(1))
            );
            assert_eq!(
                buttons[2]["custom_id"],
                format!("queue:{}", page.min(2) + 1)
            );
        }
    }

    #[test]
    fn queue_pages_handle_empty_single_and_exact_multiple() {
        let (content, rows) = render_queue_page(&[], 5);
        assert_eq!(content, "The queue is empty.");
        assert!(rows.is_empty());
        for count in [1, 10, 20] {
            let titles = vec!["Track".into(); count];
            let (_, rows) = render_queue_page(&titles, usize::MAX);
            let json = serenity::json::to_value(&rows).unwrap();
            let buttons = &json[0]["components"];
            let pages = count.div_ceil(10);
            assert_eq!(buttons[1]["label"], format!("{pages} / {pages}"));
            assert_eq!(buttons[0]["disabled"], pages == 1);
            assert_eq!(buttons[2]["disabled"], true);
        }
    }

    #[test]
    fn queue_capacity_accepts_exact_fit_and_rejects_overflow() {
        assert!(check_queue_capacity(0, 100).is_ok());
        assert!(check_queue_capacity(99, 1).is_ok());
        assert!(check_queue_capacity(95, 6).is_err());
        assert!(check_queue_capacity(100, 1).is_err());
        assert!(check_queue_capacity(0, 101).is_err());
    }
}
