//! `octocrab`-backed [`Gh`] implementation.

use super::{
    BaseLookup, CreatePrRequest, Gh, Label, PrCreated, PrDetails, PrSummary, Reviewer, UpdatePr,
};
use crate::{
    config::AutoMergeMethod,
    gh::queries::{
        ConvertToDraftInternal, ConvertToDraftResponseData, ConvertToDraftVariables,
        CreatePrInternal, CreatePrResponseData, CreatePrVariables, DisableAutoMergeInternal,
        DisableAutoMergeResponseData, DisableAutoMergeVariables, EnableAutoMergeInternal,
        EnableAutoMergeResponseData, EnableAutoMergeVariables, EnqueuePrInternal,
        EnqueuePrResponseData, EnqueuePrVariables, FindOpenPrInternal, FindOpenPrResponseData,
        FindOpenPrVariables, GetPrInternal, GetPrInternalRepositoryPullRequest, GetPrResponseData,
        GetPrVariables, LookupBaseInternal, LookupBaseResponseData, LookupBaseVariables,
        MarkReadyForReviewInternal, MarkReadyForReviewResponseData, MarkReadyForReviewVariables,
        PrWithCiStatus, PrsWithCiStatusInternal, PrsWithCiStatusResponseData,
        PrsWithCiStatusVariables, PullRequestMergeMethod, PullRequestState, RemoveLabelsInternal,
        RemoveLabelsResponseData, RemoveLabelsVariables, RequestedReviewer, UpdatePrInternal,
        UpdatePrResponseData, UpdatePrVariables,
    },
};
use anyhow::{Context, Result, anyhow};
use graphql_client::GraphQLQuery;
use octocrab::Octocrab;
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
        let (head_owner, head_branch) = head_spec
            .split_once(':')
            .ok_or_else(|| anyhow!("head_spec `{head_spec}` missing owner:branch"))?;
        let vars = FindOpenPrVariables {
            owner: owner.to_string(),
            name: repo.to_string(),
            head_ref_name: head_branch.to_string(),
        };
        let body = FindOpenPrInternal::build_query(vars);
        let data: FindOpenPrResponseData = self
            .octo
            .graphql(&body)
            .await
            .map_err(humanize)
            .with_context(|| format!("listing PRs for {owner}/{repo} head={head_spec}"))?;
        let nodes = data
            .repository
            .and_then(|r| r.pull_requests.nodes)
            .unwrap_or_default();
        let Some(pr) = nodes.into_iter().flatten().find(|p| {
            p.head_repository
                .as_ref()
                .is_some_and(|hr| hr.owner.login == head_owner)
        }) else {
            return Ok(None);
        };
        let state = match pr.state {
            PullRequestState::OPEN => "open",
            PullRequestState::CLOSED => "closed",
            PullRequestState::MERGED => "merged",
            PullRequestState::Other(_) => "unknown",
        };
        Ok(Some(PrSummary {
            number: u64::try_from(pr.number).context("PR number out of range")?,
            html_url: pr.url,
            title: pr.title,
            state: state.to_string(),
        }))
    }

    async fn lookup_base(&self, owner: &str, repo: &str, branch: &str) -> Result<BaseLookup> {
        let vars = LookupBaseVariables {
            owner: owner.to_string(),
            name: repo.to_string(),
            branch_qualified_name: format!("refs/heads/{branch}"),
        };
        let body = LookupBaseInternal::build_query(vars);
        let data: LookupBaseResponseData = self
            .octo
            .graphql(&body)
            .await
            .map_err(humanize)
            .with_context(|| format!("looking up {owner}/{repo} base={branch}"))?;
        let repo_data = data
            .repository
            .ok_or_else(|| anyhow!("repository {owner}/{repo} not found"))?;
        Ok(BaseLookup {
            repo_node_id: repo_data.id,
            branch_exists: repo_data.ref_.is_some(),
        })
    }

    async fn create_pr(&self, req: CreatePrRequest) -> Result<PrCreated> {
        let vars = CreatePrVariables {
            repo_id: req.repo_node_id,
            title: req.title,
            body: req.body,
            head_ref_name: req.head,
            base_ref_name: req.base,
            draft: req.draft,
        };
        let body = CreatePrInternal::build_query(vars);
        let data: CreatePrResponseData = self
            .octo
            .graphql(&body)
            .await
            .map_err(humanize)
            .context("creating PR")?;
        let pr = data
            .create_pull_request
            .and_then(|p| p.pull_request)
            .ok_or_else(|| anyhow!("createPullRequest returned no pull request"))?;
        Ok(PrCreated {
            number: u64::try_from(pr.number).context("PR number out of range")?,
            html_url: pr.url,
            node_id: pr.id,
            has_merge_queue: pr.merge_queue.is_some(),
        })
    }

    async fn add_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        reviewers: Vec<Reviewer>,
    ) -> Result<()> {
        if reviewers.is_empty() {
            return Ok(());
        }
        let (users, teams) = split_users_and_teams(&reviewers);
        self.octo
            .pulls(owner, repo)
            .request_reviews(pr, users, teams)
            .await
            .map_err(humanize)
            .with_context(|| format!("requesting reviews on {owner}/{repo}#{pr}"))?;
        Ok(())
    }

    async fn remove_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        reviewers: Vec<Reviewer>,
    ) -> Result<()> {
        if reviewers.is_empty() {
            return Ok(());
        }
        let (users, teams) = split_users_and_teams(&reviewers);
        self.octo
            .pulls(owner, repo)
            .remove_requested_reviewers(pr, users, teams)
            .await
            .map_err(humanize)
            .with_context(|| format!("removing review requests on {owner}/{repo}#{pr}"))?;
        Ok(())
    }

    async fn add_labels(
        &self,
        owner: &str,
        repo: &str,
        pr_num: u64,
        labels: &[String],
    ) -> Result<()> {
        if labels.is_empty() {
            return Ok(());
        }
        self.octo
            .issues(owner, repo)
            .add_labels(pr_num, labels)
            .await
            .map_err(humanize)
            .with_context(|| format!("adding labels to {owner}/{repo}#{pr_num}"))?;
        Ok(())
    }

    async fn remove_labels(&self, pr_node_id: &str, label_node_ids: &[String]) -> Result<()> {
        if label_node_ids.is_empty() {
            return Ok(());
        }
        let vars = RemoveLabelsVariables {
            pr_id: pr_node_id.to_string(),
            label_ids: label_node_ids.to_vec(),
        };
        let query = RemoveLabelsInternal::build_query(vars);
        self.octo
            .graphql::<RemoveLabelsResponseData>(&query)
            .await
            .map_err(humanize)
            .context("removing labels from PR")?;
        Ok(())
    }

    async fn update_pr(&self, req: UpdatePr) -> Result<()> {
        if req.is_noop() {
            return Ok(());
        }
        let UpdatePr {
            pr_node_id,
            title,
            body,
            base_ref_name,
        } = req;
        let vars = UpdatePrVariables {
            pr_id: pr_node_id,
            title,
            body,
            base_ref_name,
        };
        let query = UpdatePrInternal::build_query(vars);
        self.octo
            .graphql::<UpdatePrResponseData>(&query)
            .await
            .map_err(humanize)
            .context("updating PR")?;
        Ok(())
    }

    async fn set_draft(&self, pr_node_id: &str, draft: bool) -> Result<()> {
        if draft {
            let vars = ConvertToDraftVariables {
                pr_id: pr_node_id.to_string(),
            };
            let body = ConvertToDraftInternal::build_query(vars);
            self.octo
                .graphql::<ConvertToDraftResponseData>(&body)
                .await
                .map_err(humanize)
                .context("converting PR to draft")?;
        } else {
            let vars = MarkReadyForReviewVariables {
                pr_id: pr_node_id.to_string(),
            };
            let body = MarkReadyForReviewInternal::build_query(vars);
            self.octo
                .graphql::<MarkReadyForReviewResponseData>(&body)
                .await
                .map_err(humanize)
                .context("marking PR ready for review")?;
        }
        Ok(())
    }

    async fn disable_auto_merge(&self, pr_node_id: &str) -> Result<()> {
        let vars = DisableAutoMergeVariables {
            pr_id: pr_node_id.to_string(),
        };
        let body = DisableAutoMergeInternal::build_query(vars);
        self.octo
            .graphql::<DisableAutoMergeResponseData>(&body)
            .await
            .map_err(humanize)
            .context("disabling auto-merge")?;
        Ok(())
    }

    async fn get_pr(&self, owner: &str, repo: &str, number: u64, body: bool) -> Result<PrDetails> {
        let vars = GetPrVariables {
            owner: owner.to_string(),
            name: repo.to_string(),
            number: i64::try_from(number).context("PR number out of range")?,
            body,
        };
        let body = GetPrInternal::build_query(vars);
        let data = self
            .octo
            .graphql::<GetPrResponseData>(&body)
            .await
            .map_err(humanize)
            .with_context(|| format!("fetching PR #{number} on {owner}/{repo}"))?;
        let GetPrInternalRepositoryPullRequest {
            id,
            number,
            title,
            url,
            head_ref_name,
            head_ref_oid,
            merge_queue,
            head_repository,
            labels,
            is_draft,
            auto_merge_request,
            review_requests,
            body,
        } = data
            .repository
            .and_then(|r| r.pull_request)
            .ok_or_else(|| anyhow!("PR #{number} not found on {owner}/{repo}"))?;
        let (head_user_login, head_repo_name) = match head_repository {
            Some(hr) => (Some(hr.owner.login), Some(hr.name)),
            None => (None, None),
        };
        Ok(PrDetails {
            number: u64::try_from(number).context("PR number out of range")?,
            title,
            is_draft,
            html_url: url,
            head_ref: head_ref_name,
            head_sha: head_ref_oid,
            head_user_login,
            head_repo_name,
            graphql_node_id: id,
            in_merge_queue: merge_queue.is_some(),
            body: if body.is_empty() { None } else { Some(body) },
            labels: labels
                .and_then(|labels| labels.nodes)
                .map(|nodes| {
                    nodes
                        .into_iter()
                        .flatten()
                        .map(|label| Label {
                            name: label.name,
                            node_id: label.id,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            auto_merge: auto_merge_request.is_some(),
            auto_merge_method: auto_merge_request.and_then(Into::into),
            reviewers: review_requests
                .and_then(|requests| requests.nodes)
                .map(|nodes| {
                    nodes
                        .into_iter()
                        .flatten()
                        .filter_map(|node| node.requested_reviewer)
                        .filter_map(|node| {
                            let slug = match node {
                                RequestedReviewer::User(user) => user.login,
                                RequestedReviewer::Bot(clanker) => clanker.login,
                                RequestedReviewer::Mannequin(mannequin) => mannequin.login,
                                RequestedReviewer::Team(team) => team.combined_slug,
                            };
                            Reviewer::parse(&slug)
                                .inspect_err(|e| {
                                    log::warn!("dropping unparseable reviewer `{slug}`: {e}");
                                })
                                .ok()
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    async fn enable_auto_merge(
        &self,
        pr_node_id: &str,
        has_merge_queue: bool,
        method: AutoMergeMethod,
    ) -> Result<()> {
        if has_merge_queue {
            let vars = EnqueuePrVariables {
                pr_id: pr_node_id.to_string(),
            };
            let body = EnqueuePrInternal::build_query(vars);
            self.octo
                .graphql::<EnqueuePrResponseData>(&body)
                .await
                .map_err(humanize)
                .context("enqueuing PR for merge")?;
        } else {
            let vars = EnableAutoMergeVariables {
                pr_id: pr_node_id.to_string(),
                method: to_graphql_method(method),
            };
            let body = EnableAutoMergeInternal::build_query(vars);
            self.octo
                .graphql::<EnableAutoMergeResponseData>(&body)
                .await
                .map_err(humanize)
                .context("enabling auto-merge")?;
        }
        Ok(())
    }

    async fn local_pulls(
        &self,
        owner: &str,
        repo: &str,
        branches: &[String],
    ) -> Result<Vec<PrWithCiStatus>> {
        if branches.is_empty() {
            return Ok(Vec::new());
        }

        let mut out: Vec<PrWithCiStatus> = Vec::new();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for search_query in build_search_queries(owner, repo, branches) {
            let vars = PrsWithCiStatusVariables {
                query: search_query,
            };
            let body = PrsWithCiStatusInternal::build_query(vars);
            let batch: Vec<PrWithCiStatus> = self
                .octo
                .graphql::<PrsWithCiStatusResponseData>(&body)
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

/// Split reviewers into `(user_logins, team_names)` as the REST review-request
/// endpoint expects (team names without their org prefix).
fn split_users_and_teams(reviewers: &[Reviewer]) -> (Vec<String>, Vec<String>) {
    let (teams, users): (Vec<&Reviewer>, Vec<&Reviewer>) =
        reviewers.iter().partition(|r| r.team_name().is_some());
    let user_logins = users.into_iter().map(|r| r.slug().to_string()).collect();
    let team_names = teams
        .into_iter()
        .filter_map(|r| r.team_name().map(str::to_string))
        .collect();
    (user_logins, team_names)
}

fn to_graphql_method(m: AutoMergeMethod) -> PullRequestMergeMethod {
    match m {
        AutoMergeMethod::Merge => PullRequestMergeMethod::MERGE,
        AutoMergeMethod::Squash => PullRequestMergeMethod::SQUASH,
        AutoMergeMethod::Rebase => PullRequestMergeMethod::REBASE,
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
