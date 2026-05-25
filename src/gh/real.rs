//! `octocrab`-backed [`Gh`] implementation.

use super::{CreatePrRequest, Gh, PrCreated, PrDetails, PrSummary};
use crate::{
    config::AutoMergeMethod,
    gh::prs_with_ci_status::{PrWithCiStatus, PrsWithCiStatusResponseData},
};
use anyhow::{Context, Result, anyhow};
use octocrab::{Octocrab, params};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;

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
            node_id: pr.node_id,
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

    async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<PrDetails> {
        let pr = match self.octo.pulls(owner, repo).get(number).await {
            Ok(pr) => pr,
            Err(e) if is_not_found(&e) => {
                return Err(anyhow!("PR #{number} not found on {owner}/{repo}"));
            }
            Err(e) => {
                return Err(humanize(e))
                    .with_context(|| format!("fetching PR #{number} on {owner}/{repo}"));
            }
        };
        Ok(PrDetails {
            number: pr.number,
            title: pr.title,
            html_url: pr.html_url.to_string(),
            head_ref: pr.head.ref_field.clone(),
            head_sha: pr.head.sha.clone(),
            head_user_login: pr.head.user.as_ref().map(|u| u.login.clone()),
            head_repo_name: pr.head.repo.as_ref().map(|r| r.name.clone()),
            graphql_node_id: pr.node_id,
        })
    }

    async fn enable_auto_merge(&self, pr_node_id: &str, method: AutoMergeMethod) -> Result<()> {
        const MUTATION: &str = include_str!("./enable_auto_merge.gql");

        let payload = json!({
            "query": MUTATION,
            "variables": {
                "pr_id": pr_node_id,
                "method": method.as_graphql(),
            },
        });
        self.octo
            .graphql::<serde_json::Value>(&payload)
            .await
            .map_err(humanize)
            .context("enabling auto-merge")?;
        Ok(())
    }

    async fn local_pulls(
        &self,
        owner: &str,
        repo: &str,
        branches: &[String],
    ) -> Result<Vec<PrWithCiStatus>> {
        const QUERY: &str = include_str!("./prs_with_ci_status.gql");

        if branches.is_empty() {
            return Ok(Vec::new());
        }

        let mut out: Vec<PrWithCiStatus> = Vec::new();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for search_query in build_search_queries(owner, repo, branches) {
            let payload = json!({
                "query": QUERY,
                "variables": { "query": search_query },
            });
            let batch: Vec<PrWithCiStatus> = self
                .octo
                .graphql::<PrsWithCiStatusResponseData>(&payload)
                .await
                .map_err(humanize)
                .context("fetching local PRs")?
                .into();
            for pr in batch {
                if seen.insert(pr.number) {
                    out.push(pr);
                }
            }
        }
        Ok(out)
    }
}

/// GitHub's search query string limit is documented as 256 chars for some
/// endpoints but issue/PR search tolerates more in practice. We cap well
/// below the worst-case so a single oversized branch name can't break a
/// whole batch.
const MAX_SEARCH_QUERY_LEN: usize = 900;

/// Split `branches` into one or more search query strings of the form
/// `repo:owner/repo is:pr is:open head:b1 head:b2 ...`, each under
/// [`MAX_SEARCH_QUERY_LEN`].
fn build_search_queries(owner: &str, repo: &str, branches: &[String]) -> Vec<String> {
    let prefix = format!("repo:{owner}/{repo} is:pr");
    let mut queries = Vec::new();
    let mut current = prefix.clone();
    for branch in branches {
        let addition = format!(" head:{branch}");
        if current.len() + addition.len() > MAX_SEARCH_QUERY_LEN && current != prefix {
            queries.push(std::mem::replace(&mut current, prefix.clone()));
        }
        current.push_str(&addition);
    }
    if current != prefix {
        queries.push(current);
    }
    queries
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
            "GitHub rejected the token (401). Refresh your `gh_askpass` output or generate a new PAT. See README \"GitHub token permissions\" for required scopes."
        ),
        403 => anyhow!(
            "GitHub denied the request (403): {message}. Check the token's permissions (see README \"GitHub token permissions\") and whether you've hit the API rate limit."
        ),
        422 => anyhow!(
            "GitHub rejected the request (422): {message}. Common causes: the head branch is not pushed, there are no commits between head and base, or a PR already exists."
        ),
        _ => anyhow!("GitHub error ({status}): {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_branches_produces_no_queries() {
        assert!(build_search_queries("o", "r", &[]).is_empty());
    }

    #[test]
    fn single_query_when_under_limit() {
        let qs = build_search_queries("o", "r", &["a".into(), "b".into()]);
        assert_eq!(qs, vec!["repo:o/r is:pr head:a head:b".to_string()]);
    }

    #[test]
    fn splits_into_batches_when_over_limit() {
        let long = "x".repeat(200);
        let branches: Vec<String> = (0..10).map(|i| format!("{long}-{i}")).collect();
        let qs = build_search_queries("o", "r", &branches);
        assert!(qs.len() >= 2, "expected multiple batches, got {qs:?}");
        for q in &qs {
            assert!(q.len() <= MAX_SEARCH_QUERY_LEN, "batch too long: {q}");
            assert!(q.starts_with("repo:o/r is:pr"));
        }
    }
}
