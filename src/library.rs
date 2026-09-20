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

pub struct Library {
    pub root: PathBuf,
    pub tracks: Vec<Track>,
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
        Ok(Self {
            root,
            tracks,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scans_nested_audio_and_resolves_unambiguous_search() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("Album")).unwrap();
        fs::write(dir.path().join("Album/One.MP3"), []).unwrap();
        fs::write(dir.path().join("Album/Two.flac"), []).unwrap();
        fs::write(dir.path().join("cover.jpg"), []).unwrap();
        let lib = Library::scan(dir.path()).unwrap();
        assert_eq!(lib.tracks.len(), 2);
        assert_eq!(lib.resolve("ONE").unwrap().label, "Album/One.MP3");
        assert!(lib.resolve("Album").is_err());
        assert!(lib.resolve("").is_err());
        assert!(lib.resolve("../../etc/passwd").is_err());
        assert!(lib.resolve("track:999").is_err());
        assert_eq!(lib.resolve("track:1").unwrap().label, "Album/Two.flac");
    }

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = id.to_vec();
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(data);
        if !data.len().is_multiple_of(2) {
            bytes.push(0);
        }
        bytes
    }

    fn tagged_wav(path: &Path, tags: &[(&str, &str)], riff_album: Option<&str>) {
        let mut frames = Vec::new();
        for (key, value) in tags {
            frames.extend(key.as_bytes());
            frames.extend(((value.len() + 1) as u32).to_be_bytes());
            frames.extend([0, 0, 0]);
            frames.extend(value.as_bytes());
        }
        let mut bytes = Vec::new();
        if !frames.is_empty() {
            bytes.extend(b"ID3\x03\x00\x00");
            let size = frames.len() as u32;
            bytes.extend([21, 14, 7, 0].map(|shift| ((size >> shift) & 0x7f) as u8));
            bytes.extend(frames);
        }
        let mut wave = b"WAVE".to_vec();
        let mut format = Vec::new();
        format.extend(1_u16.to_le_bytes());
        format.extend(1_u16.to_le_bytes());
        format.extend(48_000_u32.to_le_bytes());
        format.extend(96_000_u32.to_le_bytes());
        format.extend(2_u16.to_le_bytes());
        format.extend(16_u16.to_le_bytes());
        wave.extend(chunk(b"fmt ", &format));
        if let Some(album) = riff_album {
            let mut info = b"INFO".to_vec();
            info.extend(chunk(b"IPRD", format!("{album}\0").as_bytes()));
            wave.extend(chunk(b"LIST", &info));
        }
        wave.extend(chunk(b"data", &[0; 32]));
        bytes.extend(chunk(b"RIFF", &wave));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn album_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        for (file, title, artist, disc, track, performer) in [
            ("one/z.wav", "Live", "Various", "1/2", "2/12", "Singer A"),
            ("two/a.wav", "Live", "Various", "2", "1", "Singer B"),
            ("root.wav", "Live", "Various", "1", "1", "Singer C"),
            ("one/b.wav", "Live", "Various", "1", "10", "Singer D"),
            ("one/c.wav", "Live", "Various", "1", "bad", "Singer E"),
            ("one/d.wav", "Live", "Various", "1", "0", "Singer F"),
            ("one/missing.wav", "Live", "Various", "", "", "Singer G"),
            ("one/other.wav", "Live", "Other", "", "", "Singer A"),
            ("deluxe.wav", "Live Deluxe", "", "", "", "Solo"),
            ("studio.wav", "Studio", "", "", "2", "Singer A"),
            ("elsewhere/studio.wav", "Studio", "", "", "1", "Singer B"),
        ] {
            tagged_wav(
                &dir.path().join(file),
                &[
                    ("TALB", title),
                    ("TPE2", artist),
                    ("TPOS", disc),
                    ("TRCK", track),
                    ("TPE1", performer),
                ],
                None,
            );
        }
        tagged_wav(&dir.path().join("untagged.wav"), &[], None);
        tagged_wav(&dir.path().join("blank.wav"), &[("TALB", "   ")], None);
        fs::write(dir.path().join("empty.mp3"), []).unwrap();
        fs::write(dir.path().join("corrupt.flac"), b"not audio").unwrap();
        let lib = Library::scan(dir.path()).unwrap();
        (dir, lib)
    }

    #[test]
    fn album_search_is_sorted_filtered_and_keeps_global_ids() {
        let (_dir, lib) = album_library();
        assert_eq!(
            lib.search_albums("", usize::MAX),
            vec![
                (0, "Live Deluxe".into()),
                (1, "Live — Other".into()),
                (2, "Live — Various".into()),
                (3, "Studio".into()),
            ]
        );
        assert_eq!(
            lib.search_albums(" LIVE ", 1),
            vec![(0, "Live Deluxe".into())]
        );
        assert_eq!(lib.search_albums("studio", 10), vec![(3, "Studio".into())]);
        assert_eq!(lib.tracks.len(), 15);
        assert_eq!(lib.resolve("empty.mp3").unwrap().label, "empty.mp3");
        assert!(lib.search_albums("", 0).is_empty());
        assert!(lib.search_albums("missing", 10).is_empty());
    }

    #[test]
    fn album_resolution_prefers_exact_labels_and_preserves_track_order() {
        let (_dir, lib) = album_library();
        for query in ["album:2", " live — VARIOUS "] {
            let (label, tracks) = lib.resolve_album(query).unwrap();
            assert_eq!(label, "Live — Various");
            assert_eq!(
                tracks
                    .iter()
                    .map(|track| track.label.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "root.wav",
                    "one/z.wav",
                    "one/b.wav",
                    "one/c.wav",
                    "one/d.wav",
                    "two/a.wav",
                    "one/missing.wav"
                ]
            );
            assert!(tracks.iter().all(|track| track.path.is_file()));
        }
        assert_eq!(lib.resolve_album("DELUXE").unwrap().0, "Live Deluxe");
        for query in ["Studio", "album:3"] {
            let (label, tracks) = lib.resolve_album(query).unwrap();
            assert_eq!(label, "Studio");
            assert_eq!(
                tracks
                    .iter()
                    .map(|track| track.label.as_str())
                    .collect::<Vec<_>>(),
                vec!["elsewhere/studio.wav", "studio.wav"]
            );
        }
    }

    #[test]
    fn album_resolution_rejects_empty_ambiguous_missing_and_invalid_ids() {
        let (_dir, lib) = album_library();
        for query in [
            "",
            "  ",
            "live",
            "Artist",
            "missing",
            "Empty",
            "album:",
            "album:nope",
            "album:-1",
            "album:999",
            "album:999999999999999999999999999999",
        ] {
            assert!(lib.resolve_album(query).is_err(), "accepted {query:?}");
        }
        let empty = tempfile::tempdir().unwrap();
        let lib = Library::scan(empty.path()).unwrap();
        assert!(lib.search_albums("", 10).is_empty());
        assert!(lib.resolve_album(".").is_err());
        assert!(lib.resolve_album("album:0").is_err());
    }

    #[test]
    fn reads_container_metadata_and_prefers_it_to_probe_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tagged.wav");
        tagged_wav(&path, &[], Some("Container Album"));
        assert_eq!(
            AlbumTags::read(&path).unwrap().title.as_deref(),
            Some("Container Album")
        );
        tagged_wav(
            &path,
            &[("TALB", "External"), ("TPE2", "Band"), ("TRCK", "03/10")],
            Some("Container Album"),
        );
        let tags = AlbumTags::read(&path).unwrap();
        assert_eq!(tags.title.as_deref(), Some("Container Album"));
        assert_eq!(tags.artist.as_deref(), Some("Band"));
        assert_eq!(tags.track, Some(3));
    }

    #[test]
    fn exact_labels_win_over_partial_matches_but_collisions_are_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        for (file, title, artist) in [
            ("a.wav", "Live", ""),
            ("b.wav", "Live Deluxe", ""),
            ("c.wav", "LIVE", ""),
        ] {
            tagged_wav(
                &dir.path().join(file),
                &[("TALB", title), ("TPE2", artist)],
                None,
            );
        }
        let lib = Library::scan(dir.path()).unwrap();
        assert!(lib.resolve_album("live").is_err());
        assert!(lib.resolve_album("album:0").is_ok());
        fs::remove_file(dir.path().join("c.wav")).unwrap();
        let lib = Library::scan(dir.path()).unwrap();
        assert_eq!(lib.resolve_album(" LIVE ").unwrap().0, "Live");
    }

    #[test]
    fn random_track_handles_empty_and_single_track_libraries() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Library::scan(dir.path()).unwrap().random_track().is_err());
        fs::write(dir.path().join("only.mp3"), []).unwrap();
        let lib = Library::scan(dir.path()).unwrap();
        for _ in 0..10 {
            let track = lib.random_track().unwrap();
            assert_eq!(track.label, "only.mp3");
            assert_eq!(track.path, lib.tracks[0].path);
        }
    }

    #[test]
    fn random_track_always_returns_an_indexed_track() {
        let (_dir, lib) = album_library();
        for _ in 0..100 {
            let track = lib.random_track().unwrap();
            assert!(
                lib.tracks
                    .iter()
                    .any(|indexed| { indexed.path == track.path && indexed.label == track.label })
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinks_outside_library() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.mp3"), []).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.mp3"),
            dir.path().join("link.mp3"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked-dir")).unwrap();
        assert!(Library::scan(dir.path()).unwrap().tracks.is_empty());
    }
}

#[cfg(test)]
mod audio_tests {
    use songbird::input::{
        File, Input,
        codecs::{get_codec_registry, get_probe},
    };
    use std::io::Write;

    #[tokio::test]
    async fn local_wav_is_playable_and_corrupt_audio_is_rejected() {
        let mut wav = tempfile::NamedTempFile::new().unwrap();
        let samples = 4_800_u32;
        let bytes = samples * 2;
        wav.write_all(b"RIFF").unwrap();
        wav.write_all(&(36 + bytes).to_le_bytes()).unwrap();
        wav.write_all(b"WAVEfmt ").unwrap();
        wav.write_all(&16_u32.to_le_bytes()).unwrap();
        wav.write_all(&1_u16.to_le_bytes()).unwrap();
        wav.write_all(&1_u16.to_le_bytes()).unwrap();
        wav.write_all(&48_000_u32.to_le_bytes()).unwrap();
        wav.write_all(&96_000_u32.to_le_bytes()).unwrap();
        wav.write_all(&2_u16.to_le_bytes()).unwrap();
        wav.write_all(&16_u16.to_le_bytes()).unwrap();
        wav.write_all(b"data").unwrap();
        wav.write_all(&bytes.to_le_bytes()).unwrap();
        wav.write_all(&vec![0; bytes as usize]).unwrap();
        let wav_path = wav.path().to_path_buf();
        let input: Input = File::new(wav_path).into();
        let input = input
            .make_playable_async(get_codec_registry(), get_probe())
            .await
            .unwrap();
        assert!(input.is_playable());

        let mut corrupt = tempfile::NamedTempFile::new().unwrap();
        corrupt.write_all(b"not an audio file").unwrap();
        let corrupt_path = corrupt.path().to_path_buf();
        let input: Input = File::new(corrupt_path).into();
        assert!(
            input
                .make_playable_async(get_codec_registry(), get_probe())
                .await
                .is_err()
        );
    }
}
