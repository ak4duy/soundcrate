use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serenity::all::*;

use crate::Handler;

use super::{library::Track, short};

type GuildPlaylists = BTreeMap<String, Vec<String>>;

pub(crate) struct Playlists {
    directory: PathBuf,
    guilds: BTreeMap<u64, GuildPlaylists>,
}

impl Playlists {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).context("Cannot create PLAYLIST_DIR")?;
        let path = directory.join("playlists.json");
        let guilds = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("Invalid playlists.json; restore a backup before starting")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error).context("Cannot read playlists.json"),
        };
        Ok(Self {
            directory: directory.to_owned(),
            guilds,
        })
    }

    fn save(&mut self, guild: u64, playlists: GuildPlaylists) -> Result<()> {
        let mut next = self.guilds.clone();
        next.insert(guild, playlists);
        let write = || -> Result<()> {
            let bytes = serde_json::to_vec_pretty(&next)?;
            let temporary = self.directory.join("playlists.json.tmp");
            let mut file = File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.directory.join("playlists.json"))?;
            Ok(())
        };
        write().map_err(|error| {
            tracing::error!(%error, "Could not save playlists");
            anyhow::anyhow!("Could not save the playlist. Check PLAYLIST_DIR permissions and free space; no changes were applied.")
        })?;
        self.guilds = next;
        #[cfg(unix)]
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                tracing::error!(%error, "Could not sync playlist directory");
                anyhow::anyhow!("Playlist updated, but storage could not confirm crash durability. Check PLAYLIST_DIR and back up playlists.json.")
            })?;
        Ok(())
    }
}

pub(super) fn definition() -> CreateCommand {
    let mut command = CreateCommand::new("playlist")
        .description("Manage server playlists from the local library")
        .dm_permission(false);
    for (action, description) in [
        ("create", "Create an empty playlist"),
        ("add", "Append a local track to a playlist"),
        ("remove", "Remove a track from a playlist"),
        ("play", "Play or queue a playlist in saved order"),
        ("show", "Show the saved tracks in a playlist"),
        ("delete", "Delete a playlist"),
    ] {
        let mut option =
            CreateCommandOption::new(CommandOptionType::SubCommand, action, description)
                .add_sub_option(
                    CreateCommandOption::new(CommandOptionType::String, "name", "Playlist name")
                        .required(true)
                        .min_length(1)
                        .max_length(60)
                        .set_autocomplete(action != "create"),
                );
        if matches!(action, "add" | "remove") {
            option = option.add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "track",
                    "Search a track or enter its library-relative path",
                )
                .required(true)
                .set_autocomplete(true),
            );
        }
        command = command.add_option(option);
    }
    command
}

pub(super) fn subcommand(cmd: &CommandInteraction) -> Result<&str> {
    Ok(cmd
        .data
        .options
        .first()
        .context("Choose a playlist action.")?
        .name
        .as_str())
}

fn option<'a>(cmd: &'a CommandInteraction, name: &str) -> Option<&'a str> {
    let CommandDataOptionValue::SubCommand(options) = &cmd.data.options.first()?.value else {
        return None;
    };
    options.iter().find(|o| o.name == name)?.value.as_str()
}

fn name(cmd: &CommandInteraction) -> Result<String> {
    let value = option(cmd, "name")
        .context("Enter a playlist name.")?
        .trim()
        .to_lowercase();
    if value.is_empty() || value.chars().count() > 60 || value.chars().any(char::is_control) {
        bail!("Use a playlist name of 1–60 characters without control characters.");
    }
    Ok(value)
}

fn reference(handler: &Handler, track: &Track) -> Result<String> {
    Ok(track
        .path
        .strip_prefix(&handler.library.root)?
        .to_str()
        .context("This filename is not valid UTF-8; rename it before adding it.")?
        .to_owned())
}

fn resolve_saved(handler: &Handler, reference: &str) -> Result<Track> {
    let path = Path::new(reference);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || reference.is_empty()
    {
        bail!("Invalid saved track path. Remove this playlist entry and add it again.");
    }
    let absolute = handler.library.root.join(path);
    handler.library.tracks.iter().find(|track| track.path == absolute).cloned()
        .with_context(|| format!("Track is missing from the library: {}. Restore it and restart, or remove it from the playlist. Nothing was added.", short(reference, 150)))
}

pub(super) async fn tracks(
    handler: &Handler,
    cmd: &CommandInteraction,
) -> Result<(String, Vec<Track>)> {
    let guild = cmd.guild_id.context("Use this command in a server.")?.get();
    let name = name(cmd)?;
    let saved = handler
        .playlists
        .lock()
        .await
        .guilds
        .get(&guild)
        .and_then(|lists| lists.get(&name))
        .cloned()
        .context("Playlist not found. Use /playlist create first.")?;
    if saved.is_empty() {
        bail!("This playlist is empty. Use /playlist add first.");
    }
    let tracks = saved
        .iter()
        .map(|path| resolve_saved(handler, path))
        .collect::<Result<_>>()?;
    Ok((name, tracks))
}

pub(super) async fn show(
    handler: &Handler,
    cmd: &CommandInteraction,
) -> Result<EditInteractionResponse> {
    let guild = cmd.guild_id.context("Use this command in a server.")?;
    page(handler, guild, &name(cmd)?, 0).await
}

pub(super) async fn page(
    handler: &Handler,
    guild: GuildId,
    name: &str,
    requested_page: usize,
) -> Result<EditInteractionResponse> {
    let store = handler.playlists.lock().await;
    let entries = store
        .guilds
        .get(&guild.get())
        .and_then(|lists| lists.get(name))
        .context("Playlist not found. It may have been deleted; use /playlist show again.")?;
    let (description, components) = if entries.is_empty() {
        ("This playlist is empty. Use /playlist add.".into(), vec![])
    } else {
        let pages = entries.len().div_ceil(10);
        let page = requested_page.min(pages - 1);
        let count = entries.len();
        let mut lines = vec![format!(
            "**{count} {}**",
            if count == 1 { "track" } else { "tracks" }
        )];
        for (index, path) in entries.iter().enumerate().skip(page * 10).take(10) {
            lines.push(format!("{}. {}", index + 1, short(path, 130)));
        }
        (
            lines.join("\n"),
            vec![super::queue::page_buttons("playlist", page, pages)],
        )
    };
    Ok(EditInteractionResponse::new()
        .content("")
        .embed(
            CreateEmbed::new()
                .title(name)
                .description(description)
                .color(0x4BFF9A),
        )
        .components(components)
        .allowed_mentions(CreateAllowedMentions::new()))
}

pub(super) async fn manage(handler: &Handler, cmd: &CommandInteraction) -> Result<String> {
    let guild = cmd.guild_id.context("Use this command in a server.")?.get();
    let name = name(cmd)?;
    let action = subcommand(cmd)?.to_owned();
    let raw_query = option(cmd, "track").unwrap_or("");
    let exact = raw_query.starts_with("path:");
    let query = raw_query
        .strip_prefix("path:")
        .unwrap_or(raw_query)
        .to_owned();
    let added = if action == "add" {
        let track = if exact {
            resolve_saved(handler, &query)?
        } else {
            resolve_saved(handler, &query).or_else(|_| handler.library.resolve(&query))?
        };
        Some(reference(handler, &track)?)
    } else {
        None
    };
    let store = handler.playlists.clone();
    tokio::task::spawn_blocking(move || {
        let mut store = store.blocking_lock();
        let mut lists = store.guilds.get(&guild).cloned().unwrap_or_default();
        let label = short(&name, 60);
        if action == "create" {
            if lists.contains_key(&name) {
                bail!("Playlist **{label}** already exists.");
            }
            if lists.len() >= 100 {
                bail!("This server already has 100 playlists. Delete one first.");
            }
            lists.insert(name.clone(), Vec::new());
        } else {
            let entries = lists.get_mut(&name).context("Playlist not found. Use /playlist create first.")?;
            match action.as_str() {
                "add" => {
                    let path = added.context("Choose a track.")?;
                    if entries.contains(&path) {
                        bail!("That track is already in this playlist.");
                    }
                    if entries.len() >= 100 {
                        bail!("A playlist can hold at most 100 tracks.");
                    }
                    entries.push(path);
                }
                "remove" => {
                    let index = if let Some(index) = entries.iter().position(|path| path == &query) {
                        index
                    } else {
                        if exact { bail!("That saved track is no longer in this playlist. Search again."); }
                        if query.is_empty() { bail!("Choose a track to remove."); }
                        let matches: Vec<_> = entries.iter().enumerate()
                            .filter(|(_, path)| path.to_lowercase().contains(&query.to_lowercase()))
                            .map(|(index, _)| index).collect();
                        match matches.as_slice() {
                            [index] => *index,
                            [] => bail!("That track is not in this playlist."),
                            _ => bail!("Several saved tracks match. Choose autocomplete or enter the exact library-relative path."),
                        }
                    };
                    entries.remove(index);
                }
                "delete" => { lists.remove(&name); }
                _ => bail!("Unknown playlist action."),
            }
        }
        store.save(guild, lists)?;
        Ok(match action.as_str() {
            "create" => format!("Created playlist **{label}**."),
            "delete" => format!("Deleted playlist **{label}**."),
            "add" => format!("Added track to **{label}**."),
            _ => format!("Removed track from **{label}**."),
        })
    }).await?
}

pub(super) async fn autocomplete(
    handler: &Handler,
    cmd: &CommandInteraction,
) -> CreateAutocompleteResponse {
    let mut response = CreateAutocompleteResponse::new();
    let Some(guild) = cmd.guild_id else {
        return response;
    };
    let Some(focused) = cmd.data.autocomplete() else {
        return response;
    };
    let query = focused.value.to_lowercase();
    let store = handler.playlists.lock().await;
    let lists = store.guilds.get(&guild.get());
    let choices: Vec<String> = if focused.name == "name" {
        lists
            .into_iter()
            .flat_map(|lists| lists.keys())
            .filter(|name| name.contains(&query))
            .take(25)
            .cloned()
            .collect()
    } else if subcommand(cmd).ok() == Some("remove") {
        name(cmd)
            .ok()
            .and_then(|name| lists.and_then(|lists| lists.get(&name)))
            .into_iter()
            .flatten()
            .filter(|path| path.to_lowercase().contains(&query))
            .filter(|path| path.chars().count() <= 95)
            .take(25)
            .cloned()
            .collect()
    } else {
        handler
            .library
            .tracks
            .iter()
            .filter(|track| track.label.to_lowercase().contains(&query))
            .filter_map(|track| reference(handler, track).ok())
            .filter(|path| path.chars().count() <= 95)
            .take(25)
            .collect()
    };
    for choice in choices {
        let value = if focused.name == "name" {
            choice.clone()
        } else {
            format!("path:{choice}")
        };
        response = response.add_string_choice(short(&choice, 100), value);
    }
    response
}
