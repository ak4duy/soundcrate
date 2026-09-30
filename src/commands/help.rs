use anyhow::Result;
use serenity::all::*;

use crate::Handler;

pub(super) async fn response(handler: &Handler) -> Result<EditInteractionResponse> {
    let description = {
        let commands = handler.help_commands.lock().await;

        commands
            .iter()
            .map(|command| format!("**/{}** — {}", command.name, command.description))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let description = if description.is_empty() {
        "Command help is not ready yet.".to_owned()
    } else {
        description
    };

    Ok(EditInteractionResponse::new()
        .content("")
        .embed(
            CreateEmbed::new()
                .title("Commands")
                .description(description)
                .color(0x4BFF9A),
        )
        .allowed_mentions(CreateAllowedMentions::new()))
}
