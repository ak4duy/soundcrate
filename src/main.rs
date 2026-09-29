mod commands;

use std::{
    collections::HashMap,
    env,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use commands::{About, Library};
use serenity::{all::*, async_trait};
use songbird::SerenityInit;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[derive(Default)]
struct Session {
    titles: HashMap<String, String>,
    announcements: HashMap<String, Arc<AtomicBool>>,
    idle_since: Option<Instant>,
    shuffle_all: bool,
}

struct Handler {
    library: Arc<Library>,
    sessions: Arc<Mutex<HashMap<GuildId, Arc<Mutex<Session>>>>>,
    guild_id: Option<GuildId>,
    about: About,
}

impl Handler {
    async fn session(&self, guild: GuildId) -> Arc<Mutex<Session>> {
        self.sessions.lock().await.entry(guild).or_default().clone()
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let result = match self.guild_id {
            Some(guild) => guild.set_commands(&ctx.http, commands::definitions()).await,
            None => Command::set_global_commands(&ctx.http, commands::definitions()).await,
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
            Interaction::Autocomplete(cmd) => commands::autocomplete(self, &ctx, &cmd).await,
            Interaction::Component(component) => commands::component(self, &ctx, &component).await,
            Interaction::Command(cmd) => commands::respond(self, &ctx, &cmd).await,
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
        about: About::new()?,
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
                session
                    .announcements
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
