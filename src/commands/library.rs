use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rand::seq::IndexedRandom;
use symphonia::core::{
    io::MediaSourceStream,
    meta::{MetadataRevision, StandardTagKey, Value},
    probe::Hint,
};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub label: String,
}

#[derive(Debug, Default)]
pub struct LibraryStats {
    pub total_bytes: u64,
    pub unknown_size_tracks: usize,
    pub album_count: usize,
    pub album_tracks: usize,
    pub formats: BTreeMap<String, usize>,
}

pub struct Library {
    pub root: PathBuf,
    pub tracks: Vec<Track>,
    pub stats: LibraryStats,
    albums: Vec<Album>,
}

struct Album {
    label: String,
    tracks: Vec<usize>,
}

#[derive(Default)]
struct AlbumTags {
    title: Option<String>,
    artist: Option<String>,
    disc: Option<u32>,
    track: Option<u32>,
}

impl AlbumTags {
    fn read(path: &Path) -> Result<Self> {
        let source =
            MediaSourceStream::new(Box::new(std::fs::File::open(path)?), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
            hint.with_extension(extension);
        }
        let mut probed = symphonia::default::get_probe().format(
            &hint,
            source,
            &Default::default(),
            &Default::default(),
        )?;
        let mut tags = Self::default();
        if let Some(mut metadata) = probed.metadata.get()
            && let Some(revision) = metadata.skip_to_latest()
        {
            tags.apply(revision);
        }
        if let Some(revision) = probed.format.metadata().skip_to_latest() {
            tags.apply(revision);
        }
        Ok(tags)
    }

    fn apply(&mut self, revision: &MetadataRevision) {
        for tag in revision.tags() {
            let text = match &tag.value {
                Value::String(value) => value.trim_matches('\0').trim().to_owned(),
                Value::UnsignedInt(value) => value.to_string(),
                Value::SignedInt(value) => value.to_string(),
                _ => continue,
            };
            if text.is_empty() {
                continue;
            }
            let number = || {
                text.split('/')
                    .next()?
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
            };
            match tag.std_key {
                Some(StandardTagKey::Album) => self.title = Some(text),
                Some(StandardTagKey::AlbumArtist) => self.artist = Some(text),
                Some(StandardTagKey::DiscNumber) => self.disc = number().or(self.disc),
                Some(StandardTagKey::TrackNumber) => self.track = number().or(self.track),
                _ => {}
            }
        }
    }
}

impl Library {
    pub fn scan(root: &Path) -> Result<Self> {
        let root = root.canonicalize().context("Cannot access MUSIC_DIR")?;
        if !root.is_dir() {
            bail!("MUSIC_DIR must be a directory");
        }
        let mut tracks = Vec::new();
        let mut stats = LibraryStats::default();
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry = entry.context("Cannot read music library entry")?;
            if !entry.file_type().is_file() {
                continue;
            }
            let extension = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(
                extension.as_str(),
                "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" | "aiff" | "aif"
            ) {
                continue;
            }
            match entry.metadata() {
                Ok(metadata) => stats.total_bytes += metadata.len(),
                Err(_) => stats.unknown_size_tracks += 1,
            }
            *stats.formats.entry(extension).or_default() += 1;
            tracks.push(Track {
                path: entry.path().to_owned(),
                label: entry
                    .path()
                    .strip_prefix(&root)?
                    .to_string_lossy()
                    .into_owned(),
            });
        }
        tracks.sort_by(|a, b| a.label.cmp(&b.label));
        let mut grouped = BTreeMap::new();
        for (id, track) in tracks.iter().enumerate() {
            let tags = AlbumTags::read(&track.path).unwrap_or_default();
            if let Some(title) = tags.title {
                grouped
                    .entry((title, tags.artist))
                    .or_insert_with(Vec::new)
                    .push((tags.disc, tags.track, id));
            }
        }
        let mut albums: Vec<_> = grouped
            .into_iter()
            .map(|((title, artist), mut members)| {
                members.sort_by_key(|&(disc, track, id)| {
                    (disc.is_none(), disc, track.is_none(), track, id)
                });
                Album {
                    label: match artist {
                        Some(artist) => format!("{title} — {artist}"),
                        None => title,
                    },
                    tracks: members.into_iter().map(|(_, _, id)| id).collect(),
                }
            })
            .collect();
        albums.sort_by(|a, b| a.label.cmp(&b.label));
        stats.album_count = albums.len();
        stats.album_tracks = albums.iter().map(|album| album.tracks.len()).sum();
        Ok(Self {
            root,
            tracks,
            stats,
            albums,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(usize, &Track)> {
        let query = query.trim().to_lowercase();
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.label.to_lowercase().contains(&query))
            .take(limit)
            .collect()
    }

    pub fn search_albums(&self, query: &str, limit: usize) -> Vec<(usize, String)> {
        let query = query.trim().to_lowercase();
        self.albums
            .iter()
            .enumerate()
            .filter(|(_, album)| album.label.to_lowercase().contains(&query))
            .take(limit)
            .map(|(id, album)| (id, album.label.clone()))
            .collect()
    }

    pub fn resolve_album(&self, query: &str) -> Result<(String, Vec<Track>)> {
        let query = query.trim();
        if query.is_empty() {
            bail!("Enter an album name or choose an autocomplete result.");
        }
        let selected = if let Some(id) = query.strip_prefix("album:") {
            self.albums
                .get(id.parse::<usize>().context("Invalid album selection")?)
                .context("Album is no longer available; search again")?
        } else {
            let query = query.to_lowercase();
            let mut exact = self
                .albums
                .iter()
                .filter(|album| album.label.to_lowercase() == query);
            if let Some(album) = exact.next() {
                if exact.next().is_some() {
                    bail!("Several albums match. Choose one from autocomplete.");
                }
                album
            } else {
                let mut matches = self
                    .albums
                    .iter()
                    .filter(|album| album.label.to_lowercase().contains(&query));
                let album = matches
                    .next()
                    .context("No matching albums. Try another search.")?;
                if matches.next().is_some() {
                    bail!("Several albums match. Choose one from autocomplete.");
                }
                album
            }
        };
        Ok((
            selected.label.clone(),
            selected
                .tracks
                .iter()
                .map(|&id| self.tracks[id].clone())
                .collect(),
        ))
    }

    pub fn random_track(&self) -> Result<Track> {
        self.tracks
            .choose(&mut rand::rng())
            .cloned()
            .context("No tracks are available in the library")
    }

    pub fn resolve(&self, query: &str) -> Result<Track> {
        if let Some(id) = query.strip_prefix("track:") {
            return self
                .tracks
                .get(id.parse::<usize>().context("Invalid track selection")?)
                .cloned()
                .context("Track is no longer available; search again");
        }
        if query.trim().is_empty() {
            bail!("Enter a song name or choose an autocomplete result.");
        }
        if let Some(track) = self
            .tracks
            .iter()
            .find(|t| t.label.eq_ignore_ascii_case(query))
        {
            return Ok(track.clone());
        }
        let matches = self.search(query, 2);
        match matches.as_slice() {
            [(_, track)] => Ok((*track).clone()),
            [] => bail!("No matching tracks. Try another search."),
            _ => bail!("Several tracks match. Choose one from autocomplete."),
        }
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        size /= 1024.0;
        unit = next;
        if size < 1024.0 {
            break;
        }
    }
    format!("{size:.2} {unit}")
}

pub(super) fn library_summary(library: &Library) -> String {
    let stats = &library.stats;
    let total = library.tracks.len();
    let formats = stats
        .formats
        .iter()
        .map(|(extension, count)| format!("**{}**: {count}", extension.to_ascii_uppercase()))
        .collect::<Vec<_>>()
        .join(" — ");
    let mut content = format!(
        "**Music library**\n\n**Total items:** {total} tracks\n**Total size:** {} ({} bytes)\n**Albums:** {}\n**Tracks with album tags:** {} / {total}\n\n**Formats**\n{}",
        format_size(stats.total_bytes),
        stats.total_bytes,
        stats.album_count,
        stats.album_tracks,
        if formats.is_empty() {
            "No indexed audio files."
        } else {
            &formats
        },
    );
    if stats.unknown_size_tracks > 0 {
        content.push_str(&format!(
            "\n\nSize is incomplete: {} file(s) could not be measured.",
            stats.unknown_size_tracks
        ));
    }
    content.push_str("\n\n*Restart the bot after changing files or tags.*");
    content
}
