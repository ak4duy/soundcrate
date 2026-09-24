use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use serenity::{all::*, async_trait};
use songbird::input::File;
use tracing::{error, warn};

use crate::{Handler, Session};

use super::short;

pub(super) async fn execute(
    handler: &Handler,
    ctx: &Context,
    manager: &songbird::Songbird,
    guild: GuildId,
    user_channel: ChannelId,
    cmd: &CommandInteraction,
    session: &mut Session,
) -> Result<String> {
    let channel = cmd.channel_id;
    let name = cmd.data.name.as_str();
    let query = cmd
        .data
        .options
        .iter()
        .find(|option| option.name == "track" || option.name == "album")
        .and_then(|option| option.value.as_str());
    let (label, tracks) = match name {
        "playalbum" => handler
            .library
            .resolve_album(query.context("Choose an album.")?)?,
        "playrandom" => {
            let track = handler.library.random_track()?;
            (track.label.clone(), vec![track])
        }
        _ => {
            let track = handler.library.resolve(query.context("Choose a track.")?)?;
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
        if !canonical.starts_with(&handler.library.root) {
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
        let announced = Arc::new(AtomicBool::new(false));
        audio.events.add_event(
            songbird::events::EventData::new(
                songbird::Event::Track(songbird::TrackEvent::Play),
                NowPlaying {
                    http: ctx.http.clone(),
                    channel,
                    title: short(&track.label, 150),
                    announced: announced.clone(),
                },
            ),
            Duration::ZERO,
        );
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
        let id = handle.uuid().to_string();
        session.titles.insert(id.clone(), track.label.clone());
        session.announcements.insert(id, announced);
    }
    session.idle_since = None;
    if name == "playalbum" {
        return Ok(format!(
            "{} album **{}** — {count} tracks ({position}).",
            if position == 1 { "Playing" } else { "Queued" },
            short(&label, 150)
        ));
    }
    Ok(format!(
        "{} **{}** ({}).",
        if position == 1 { "Playing" } else { "Queued" },
        short(&label, 150),
        position,
    ))
}

fn check_queue_capacity(queued: usize, added: usize) -> Result<()> {
    let remaining = 100_usize.saturating_sub(queued);
    if added > remaining {
        bail!("Queue is full. Nothing was added.");
    }
    Ok(())
}

struct NowPlaying {
    http: Arc<Http>,
    channel: ChannelId,
    title: String,
    announced: Arc<AtomicBool>,
}

#[async_trait]
impl songbird::EventHandler for NowPlaying {
    async fn act(&self, _ctx: &songbird::EventContext<'_>) -> Option<songbird::Event> {
        if self.announced.swap(true, Ordering::Relaxed) {
            return None;
        }
        send_now_playing(&self.http, self.channel, &self.title).await;
        None
    }
}

pub(super) async fn send_now_playing(http: &Http, channel: ChannelId, title: &str) {
    let message = CreateMessage::new()
        .embed(
            CreateEmbed::new()
                .description(format!("Playing **{}**", short(title, 150)))
                .color(0x4BFF9A),
        )
        .allowed_mentions(CreateAllowedMentions::new());
    if let Err(error) = channel.send_message(http, message).await {
        warn!(%error, "Could not send now-playing message");
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
