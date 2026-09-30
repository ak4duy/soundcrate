use std::sync::atomic::Ordering;

use anyhow::{Context as _, Ok, Result, bail};
use serenity::all::*;
use songbird::Call;

use crate::{Session, commands::shuffle};

use super::play::send_now_playing;

pub(super) async fn execute(
    call: &Call,
    session: &mut Session,
    ctx: &Context,
    channel: ChannelId,
    name: &str,
    mode: Option<&str>,
) -> Result<String> {
    match name {
        "shuffle" => match mode {
            Some("all") => {
                session.shuffle_all = true;
                shuffle::shuffle_upcoming(call.queue());
                Ok("Shuffle: all".into())
            }
            Some("off") => {
                session.shuffle_all = false;
                Ok("Shuffle: off".into())
            }
            _ => bail!("Choose a shuffle mode"),
        },
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
            let queue = call.queue().current_queue();
            if queue.is_empty() {
                bail!("The queue is empty.");
            }
            let next = queue.get(1).and_then(|track| {
                let id = track.uuid().to_string();
                let title = session.titles.get(&id)?.clone();
                let announced = session.announcements.get(&id)?.clone();
                Some((title, announced))
            });
            call.queue().skip()?;
            if let Some((title, announced)) = next
                && !announced.swap(true, Ordering::Relaxed)
            {
                send_now_playing(&ctx.http, channel, &title).await;
            }
            Ok("Skipped.".into())
        }
        "stop" => {
            session.autoplay_channel = None;
            call.queue().stop();
            session.titles.clear();
            session.announcements.clear();
            session.idle_since = None;
            Ok("Queue cleared.".into())
        }
        _ => bail!("Unknown command."),
    }
}
