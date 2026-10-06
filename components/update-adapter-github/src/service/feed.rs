use error_stack::{Report, ResultExt};
use remora_update::{
    adapter::feed::{Error, ReleaseFeedAdapter, Result},
    model::{Release, Version},
};
use serde::Deserialize;

/// The latest release of a GitHub repository, from its public REST API: no
/// token, since the releases repo is public.
pub struct GithubReleaseFeedImpl {
    client: reqwest::Client,
    api: String,
    repo: String,
}

impl GithubReleaseFeedImpl {
    /// `repo` is `owner/name`.
    pub fn new(repo: &str) -> Self {
        Self::with_api("https://api.github.com", repo)
    }

    /// Against another API root, e.g. a test server.
    pub fn with_api(api: &str, repo: &str) -> Self {
        Self {
            client: client(),
            api: api.trim_end_matches('/').to_owned(),
            repo: repo.to_owned(),
        }
    }
}

pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("remora-update/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("a default reqwest client builds")
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
}

#[async_trait::async_trait]
impl ReleaseFeedAdapter for GithubReleaseFeedImpl {
    async fn latest(&self) -> Result<Release> {
        let url = format!("{}/repos/{}/releases/latest", self.api, self.repo);
        let response = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .change_context(Error::Unreachable)
            .attach_with(|| url.clone())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Report::new(Error::NoRelease).attach(url));
        }
        let response = response
            .error_for_status()
            .change_context(Error::Unreachable)
            .attach_with(|| url.clone())?;
        let latest: LatestRelease = response.json().await.change_context(Error::Parse)?;
        let version = Version::parse(&latest.tag_name).ok_or_else(|| {
            Report::new(Error::Parse).attach(format!("tag {:?} is not a version", latest.tag_name))
        })?;
        Ok(Release {
            version,
            tag: latest.tag_name,
        })
    }
}
