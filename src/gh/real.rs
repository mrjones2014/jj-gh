//! `octocrab`-backed [`Gh`] implementation.

use super::{CreatePrRequest, Gh, PrCreated, PrSummary};
use anyhow::{Context, Result, anyhow};
use octocrab::{Octocrab, params};
use secrecy::{ExposeSecret, SecretString};

/// Production [`Gh`] impl wrapping an authenticated `octocrab` client.
pub struct OctocrabGh {
    octo: Octocrab,
}

impl OctocrabGh {
    /// Construct a new client. Sets a `jj-gh/<version>` User-Agent.
    ///
    /// # Errors
    ///
    /// Propagates octocrab builder errors.
    pub fn new(token: &SecretString) -> Result<Self> {
        let octo = Octocrab::builder()
            .personal_token(token.expose_secret().to_string())
            .add_header(
                http::header::USER_AGENT,
                format!("jj-gh/{}", env!("CARGO_PKG_VERSION")),
            )
            .build()
            .context("could not build octocrab client")?;
        Ok(Self { octo })
    }
}

impl Gh for OctocrabGh {
    async fn find_open_pr(
        &self,
        owner: &str,
        repo: &str,
        head_spec: &str,
    ) -> Result<Option<PrSummary>> {
        let page = self
            .octo
            .pulls(owner, repo)
            .list()
            .state(params::State::Open)
            .head(head_spec)
            .send()
            .await
            .map_err(humanize)
            .with_context(|| format!("listing PRs for {owner}/{repo} head={head_spec}"))?;
        Ok(page.items.into_iter().next().map(|p| PrSummary {
            number: p.number,
            html_url: p.html_url.to_string(),
            title: p.title,
            state: format!("{:?}", p.state).to_lowercase(),
        }))
    }

    async fn branch_exists(&self, owner: &str, repo: &str, branch: &str) -> Result<bool> {
        match self
            .octo
            .repos(owner, repo)
            .get_ref(&params::repos::Reference::Branch(branch.into()))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if is_not_found(&e) => Ok(false),
            Err(e) => {
                Err(humanize(e)).with_context(|| format!("checking branch {owner}/{repo}/{branch}"))
            }
        }
    }

    async fn create_pr(&self, req: CreatePrRequest) -> Result<PrCreated> {
        let pr = self
            .octo
            .pulls(&req.owner, &req.repo)
            .create(&req.title, &req.head, &req.base)
            .body(&req.body)
            .draft(req.draft)
            .send()
            .await
            .map_err(humanize)
            .with_context(|| format!("creating PR on {}/{}", req.owner, req.repo))?;
        Ok(PrCreated {
            number: pr.number,
            html_url: pr.html_url.to_string(),
        })
    }

    async fn add_labels(
        &self,
        owner: &str,
        repo: &str,
        pr_num: u64,
        labels: &[String],
    ) -> Result<()> {
        self.octo
            .issues(owner, repo)
            .add_labels(pr_num, labels)
            .await
            .map_err(humanize)
            .with_context(|| format!("adding labels to {owner}/{repo}#{pr_num}"))?;
        Ok(())
    }
}

fn is_not_found(e: &octocrab::Error) -> bool {
    matches!(
        e,
        octocrab::Error::GitHub { source, .. } if source.status_code.as_u16() == 404
    )
}

/// Map an `octocrab::Error` into an `anyhow::Error` with a human-friendly message
/// for common GitHub error codes.
fn humanize(e: octocrab::Error) -> anyhow::Error {
    let octocrab::Error::GitHub { source, .. } = &e else {
        return e.into();
    };
    let status = source.status_code.as_u16();
    let message = source.message.trim();
    match status {
        401 => anyhow!(
            "GitHub rejected the token (401). Refresh your `gh_askpass` output or generate a new PAT with the `repo` scope."
        ),
        403 => anyhow!(
            "GitHub denied the request (403): {message}. Check the token's scopes (`repo`) and whether you've hit the API rate limit."
        ),
        422 => anyhow!(
            "GitHub rejected the request (422): {message}. Common causes: the head branch is not pushed, there are no commits between head and base, or a PR already exists."
        ),
        _ => anyhow!("GitHub error ({status}): {message}"),
    }
}
