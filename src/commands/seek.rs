use std::time::Duration;

use anyhow::{Context as _, Result};
use songbird::Call;

pub(super) async fn execute(call: &Call, seconds: u64) -> Result<String> {
    let track = call.queue().current().context("Nothing is playing.")?;

    let position = track
        .seek_async(Duration::from_secs(seconds))
        .await
        .context("Could not seek this track.")?;

    let seconds = position.as_secs();
    Ok(format!(
        "Seeked to **{}:{:02}**.",
        seconds / 60,
        seconds % 60,
    ))
}
