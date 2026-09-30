use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use serenity::all::*;
use tracing::warn;

use crate::{Handler, Session};

use super::short;

pub(super) async fn page(
    handler: &Handler,
    ctx: &Context,
    guild: GuildId,
    page: usize,
) -> Result<(String, Vec<CreateActionRow>)> {
    let manager = songbird::get(ctx)
        .await
        .context("Voice service unavailable")?;
    let session = handler.session(guild).await;
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

pub(super) async fn clear_choices(
    handler: &Handler,
    ctx: &Context,
    guild: GuildId,
    query: &str,
) -> Vec<(String, String)> {
    let Some(manager) = songbird::get(ctx).await else {
        return vec![];
    };
    let session = handler.session(guild).await;
    let session = session.lock().await;
    let Some(call) = manager.get(guild) else {
        return vec![];
    };
    let call = call.lock().await;
    let tracks = call.queue().current_queue();
    queue_choices(
        tracks.iter().map(|track| track.uuid().to_string()),
        &session.titles,
        query,
    )
}

fn queue_choices(
    ids: impl Iterator<Item = String>,
    titles: &HashMap<String, String>,
    query: &str,
) -> Vec<(String, String)> {
    let query = query.trim().to_lowercase();
    ids.enumerate()
        .filter_map(|(index, id)| {
            let position = (index + 1).to_string();
            let title = titles
                .get(&id)
                .map(String::as_str)
                .unwrap_or("Unknown track");
            (position.contains(&query) || title.to_lowercase().contains(&query)).then(|| {
                (
                    short(&format!("{position}. {title}"), 100),
                    format!("queue-track:{id}"),
                )
            })
        })
        .take(25)
        .collect()
}

fn queue_selection(ids: &[String], query: Option<&str>) -> Result<usize> {
    let query = query
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("Choose a queued track or enter a positive queue index from /queue.")?;
    if ids.is_empty() {
        bail!("The queue is empty.");
    }
    if let Some(id) = query.strip_prefix("queue-track:") {
        return ids
            .iter()
            .position(|candidate| candidate == id)
            .context("That track is no longer queued. Choose a track again from /clear.");
    }
    let position = query
        .parse::<usize>()
        .ok()
        .filter(|&n| n > 0 && query.bytes().all(|b| b.is_ascii_digit()))
        .context("Enter a positive queue index from /queue or select an autocomplete result.")?;
    if position > ids.len() {
        bail!(
            "Queue index out of range. Choose a track from 1 to {}.",
            ids.len()
        );
    }
    Ok(position - 1)
}

pub(super) fn clear_track(
    queue: &songbird::tracks::TrackQueue,
    session: &mut Session,
    query: Option<&str>,
) -> Result<String> {
    queue.modify_queue(|tracks| {
        let ids: Vec<_> = tracks
            .iter()
            .map(|track| track.uuid().to_string())
            .collect();
        let index = queue_selection(&ids, query)?;
        let removed = tracks
            .remove(index)
            .expect("selection checked under queue lock");
        let _ = removed.stop();
        let title = session
            .titles
            .remove(&ids[index])
            .unwrap_or_else(|| "Unknown track".into());
        session.announcements.remove(&ids[index]);
        let mut content = format!("Removed **{}** ({}).", short(&title, 150), index + 1);
        if index == 0
            && let Some(next) = tracks.front()
            && let Err(error) = next.play()
        {
            warn!(%error, "Could not start next track after /clear");
            content.push_str(" The next track could not be started; try /skip.");
        }
        Ok(content)
    })
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
    (lines.join("\n"), vec![page_buttons("queue", page, pages)])
}

pub(super) fn page_buttons(prefix: &str, page: usize, pages: usize) -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{prefix}:{}", page.saturating_sub(1)))
            .label("◄")
            .style(ButtonStyle::Secondary)
            .disabled(page == 0),
        CreateButton::new(format!("{prefix}:page"))
            .label(format!("{} / {pages}", page + 1))
            .style(ButtonStyle::Secondary)
            .disabled(true),
        CreateButton::new(format!("{prefix}:{}", page + 1))
            .label("►")
            .style(ButtonStyle::Secondary)
            .disabled(page + 1 == pages),
    ])
}
