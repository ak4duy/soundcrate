use std::{
    cmp::Ordering,
    future::Future,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_REPOSITORY: &str = "ak4duy/soundcrate";

pub struct About {
    client: reqwest::Client,
    github_token: Option<String>,
    metadata: Metadata,
    started: Instant,
    nightly: Mutex<NightlyCache>,
}

struct Metadata {
    branch: Option<&'static str>,
    date: Option<&'static str>,
    sha: Option<&'static str>,
    repository: &'static str,
}

impl Metadata {
    fn embedded() -> Self {
        Self {
            branch: nonempty(option_env!("SOUNDCRATE_BUILD_BRANCH")),
            date: nonempty(option_env!("SOUNDCRATE_BUILD_DATE")),
            sha: nonempty(option_env!("SOUNDCRATE_BUILD_SHA")),
            repository: nonempty(option_env!("SOUNDCRATE_BUILD_REPOSITORY"))
                .unwrap_or(DEFAULT_REPOSITORY),
        }
    }

    fn build_date(&self) -> Result<DateTime<Utc>> {
        parse_date(self.date.context("Build date metadata is missing")?)
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_date(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("Invalid RFC3339 date: {value}"))?
        .with_timezone(&Utc))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NightlyStatus {
    Available,
    Current,
    Newer,
    Unavailable,
}

impl NightlyStatus {
    fn compare(build: DateTime<Utc>, latest: DateTime<Utc>) -> Self {
        match latest.cmp(&build) {
            Ordering::Greater => Self::Available,
            Ordering::Equal => Self::Current,
            Ordering::Less => Self::Newer,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Available => "A newer nightly build is available.",
            Self::Current => "Up to date with the latest published nightly.",
            Self::Newer => "Running a newer build than the latest published nightly.",
            Self::Unavailable => "Nightly update check unavailable.",
        }
    }
}

#[derive(Default)]
struct NightlyCache {
    checked: Option<(Instant, NightlyStatus)>,
}

impl NightlyCache {
    async fn get_or_fetch<F, Fut>(&mut self, fetch: F) -> NightlyStatus
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<NightlyStatus>>,
    {
        if let Some((checked, status)) = self.checked
            && checked.elapsed() < CACHE_TTL
        {
            return status;
        }
        let status = match fetch().await {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "Nightly update check failed");
                NightlyStatus::Unavailable
            }
        };
        self.checked = Some((Instant::now(), status));
        status
    }
}

#[derive(Deserialize)]
struct WorkflowRuns {
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    run_started_at: String,
}

impl WorkflowRuns {
    fn latest_started(&self) -> Result<DateTime<Utc>> {
        self.workflow_runs
            .iter()
            .map(|run| parse_date(&run.run_started_at))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max()
            .context("No successful nightly workflow runs found")
    }
}

impl About {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .user_agent(concat!("soundcrate/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("Could not create the nightly update HTTP client")?,
            github_token: std::env::var("GITHUB_TOKEN")
                .ok()
                .and_then(|token| nonempty(Some(&token)).map(str::to_owned)),
            metadata: Metadata::embedded(),
            started: Instant::now(),
            nightly: Mutex::new(NightlyCache::default()),
        })
    }

    pub fn repository_url(&self) -> String {
        format!("https://github.com/{}", self.metadata.repository)
    }

    pub async fn render(&self) -> String {
        let status = if self.metadata.branch == Some("nightly") {
            Some(
                self.nightly
                    .lock()
                    .await
                    .get_or_fetch(|| async {
                        let build = self.metadata.build_date()?;
                        let mut request = self
                            .client
                            .get(format!(
                                "https://api.github.com/repos/{}/actions/workflows/docker.yml/runs",
                                self.metadata.repository
                            ))
                            .query(&[
                                ("branch", "nightly"),
                                ("status", "success"),
                                ("per_page", "100"),
                            ])
                            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                            .header("X-GitHub-Api-Version", "2022-11-28");
                        if let Some(token) = &self.github_token {
                            request = request.bearer_auth(token);
                        }
                        let runs = request
                            .send()
                            .await
                            .context("GitHub nightly workflow request failed")?
                            .error_for_status()
                            .context("GitHub nightly workflow request returned an error")?
                            .json::<WorkflowRuns>()
                            .await
                            .context("Could not decode GitHub nightly workflow runs")?;
                        Ok(NightlyStatus::compare(build, runs.latest_started()?))
                    })
                    .await,
            )
        } else {
            None
        };
        render_details(&self.metadata, self.started.elapsed(), status)
    }
}

fn render_details(metadata: &Metadata, uptime: Duration, status: Option<NightlyStatus>) -> String {
    let date = metadata
        .build_date()
        .map(|date| date.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|_| "Unknown".into());
    let sha = metadata
        .sha
        .map(|sha| sha.chars().take(7).collect::<String>())
        .unwrap_or_else(|| "Unknown".into());
    let seconds = uptime.as_secs();
    let mut text = format!(
        "Version: **{}**\n\
         Branch/ref: **{}**\n\
         Build date: **{date}**\n\
         Commit: **{sha}**\n\
         Uptime: **{}d {}h {}m {}s**\n\n\
         *{}.*",
        env!("CARGO_PKG_VERSION"),
        metadata.branch.unwrap_or("Unknown"),
        seconds / 86_400,
        seconds / 3_600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        env!("CARGO_PKG_DESCRIPTION"),
    );
    if let Some(status) = status {
        text.push('\n');
        text.push_str(status.label());
    }
    text
}
