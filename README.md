# Soundcrate

A self-hosted Discord music bot built with Rust, Serenity, and Songbird. It plays files from a folder on your machine into Discord voice channels.

## Run with Docker Compose

The app image is published to `ghcr.io/ak4duy/soundcrate`:

- `latest` - stable build from master
- `nightly` - development build from nightly

1. Create an application and bot in the [Discord Developer Portal](https://discord.com/developers/applications), then copy its bot token.
2. Invite it to your server using the `bot`. Grant **View Channels**, **Connect**, **Speak**, **Send Messages**, **Embed Links** and **Message Content Intent**.
3. Save `docker-compose.yaml` in your deployment directory. Edit `environment.DISCORD_TOKEN` with your bot token and `volumes[0].source` with the **absolute host path** containing your music.
4. Start the bot:

   ```sh
   docker compose pull
   docker compose up -d
   ```

## Commands

| Command            | Behavior                                                                                    |
| ------------------ | ------------------------------------------------------------------------------------------- |
| `/play track`      | Search local files, join your channel, and enqueue the selected track                       |
| `/playurl url`     | Play or queue audio from a direct HTTP(S) URL                                               |
| `/playalbum album` | Search tagged albums and queue all their tracks in disc/track order                         |
| `/playrandom`      | Play or queue one random library track                                                      |
| `/library`         | Show indexed track and album counts, total audio size, album-tag coverage, and file formats |
| `/queue`           | Browse the current queue                                                                    |
| `/pause`           | Pause the current track                                                                     |
| `/resume`          | Resume playback                                                                             |
| `/skip`            | Skip the current track                                                                      |
| `/clear track`     | Remove one queued track by its `/queue` index or autocomplete selection                     |
| `/stop`            | Clear the queue and disconnect                                                              |
| `/about`           | Show Soundcrate version, build details, and update status                                   |

Check that the mount is readable and inspect the indexed filenames:

```sh
docker compose run --rm bot --check-library
```

Stop the service with `docker compose down`.

## Updates and image publishing

Pull and replace the running container:

```sh
docker compose pull
docker compose up -d
```

## Local development

To explicitly build and run your local source with Docker (requires BuildKit):

```sh
docker compose -f docker-compose.yaml -f docker-compose.dev.yaml up -d --build
```

When running the Rust executable directly, pass configuration through environment variables:

```sh
MUSIC_DIR=/path/to/music cargo run -- --check-library
DISCORD_TOKEN=your_token MUSIC_DIR=/path/to/music GUILD_ID=your_server_id cargo run
```

Validation:

```sh
cargo fmt --check
cargo check --locked
cargo clippy --locked --bin soundcrate -- -D warnings
docker build -t soundcrate .
```

## Layout

- `src/main.rs`: Discord event dispatch, shared session state, idle cleanup, shutdown.
- `src/commands/mod.rs`: slash-command registration, autocomplete, dispatch, and responses.
- `src/commands/play.rs`: playback commands, now-playing and error announcements.
- `src/commands/queue.rs`: paginated queue and track removal.
- `src/commands/voice.rs`: playback controls.
- `src/commands/about.rs`: `/about` response and build update checks.
- `src/commands/library.rs`: music indexing, album search, and `/library` summary.
- `docker-compose-example.yaml`: prebuilt production image with a read-only host music mount.
- `docker-compose-dev.yaml`: explicit override for local source builds.
- `.github/workflows/docker.yml`: GHCR image publishing and build cache.
- `Dockerfile`: multi-stage build with a non-root runtime.
