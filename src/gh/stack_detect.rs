//! Reading the local `jj` graph to work out what the PRs on GitHub *should*
//! look like.
//!
//! Two questions come out of one pass over the graph, because both are
//! answered by the same per-PR lookup (`stacked_ancestor_bookmark`):
//!
//! - which PRs form a stack, bottom to top ([`LocalShape::chains`]);
//! - what each PR's base ref should be ([`LocalShape::bases`]).
//!
//! Nothing here talks to GitHub. Applying the shape is
//! [`crate::gh::stack_create`]'s job.

use crate::{gh::PrDetails, jj::Jj};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

/// One PR's proposed base-ref transition. Computed up-front so the preview,
/// the `--json` dump, and the apply step all share a single representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BasePlan {
    pub pr_number: u64,
    pub pr_node_id: String,
    pub title: String,
    pub bookmark: String,
    pub local_commit_id: String,
    pub current_base: String,
    pub proposed_base: String,
}

impl BasePlan {
    /// True when GitHub already has the base ref this plan proposes.
    #[must_use]
    pub fn is_no_change(&self) -> bool {
        self.current_base == self.proposed_base
    }
}

/// What the local graph says GitHub should look like.
#[derive(Debug, Clone, Default)]
pub struct LocalShape {
    /// Stack chains, each ordered bottom (closest to trunk) to top. Chains of
    /// a single PR are not returned: GitHub stacks need at least two PRs.
    pub chains: Vec<Vec<u64>>,
    /// One entry per PR that has a local bookmark, in input order. Includes
    /// PRs that belong to no chain, so a lone PR whose base drifted still gets
    /// retargeted.
    pub bases: Vec<BasePlan>,
}

/// Read the local graph and derive both the stack chains and the per-PR base
/// refs.
///
/// `head_to_local_commit` maps PR head branch names to the commit their
/// *local* bookmark points at, so a bookmark rebased without pushing is read
/// from its current position rather than the PR's stale remote head.
///
/// `trunk` is the fallback base for a PR with no stacked ancestor. When it is
/// `None` the PR's current base is proposed instead, which makes every plan a
/// no-change; callers that only want [`LocalShape::chains`] can pass `None`.
///
/// # Errors
///
/// Propagates jj failures.
pub async fn detect(
    prs: &[PrDetails],
    jj: &impl Jj,
    head_to_local_commit: &HashMap<String, String>,
    trunk: Option<&str>,
) -> Result<LocalShape> {
    let ancestors = ancestor_bookmarks(prs, jj, head_to_local_commit).await?;
    Ok(LocalShape {
        chains: chains(prs, &ancestors),
        bases: bases(prs, &ancestors, head_to_local_commit, trunk),
    })
}

/// The bookmark on each PR's closest bookmarked ancestor commit, or `None`
/// when the PR has no local bookmark or nothing bookmarked sits below it.
async fn ancestor_bookmarks(
    prs: &[PrDetails],
    jj: &impl Jj,
    head_to_local_commit: &HashMap<String, String>,
) -> Result<HashMap<u64, Option<String>>> {
    let mut ancestors = HashMap::<u64, Option<String>>::with_capacity(prs.len());
    for pr in prs {
        let ancestor = match head_to_local_commit.get(&pr.head_ref) {
            Some(commit) => jj.stacked_ancestor_bookmark(commit).await?,
            None => None,
        };
        ancestors.insert(pr.number, ancestor);
    }
    Ok(ancestors)
}

/// Walk each bottom PR upwards through its successors to build the chains.
fn chains(prs: &[PrDetails], ancestors: &HashMap<u64, Option<String>>) -> Vec<Vec<u64>> {
    let head_to_pr = prs
        .iter()
        .map(|pr| (pr.head_ref.clone(), pr.number))
        .collect::<HashMap<String, u64>>();
    let ancestor_of = |number: u64| ancestors.get(&number).and_then(Option::as_ref);

    // A bottom PR is one whose ancestor bookmark is not itself a PR in the set.
    let bottoms = prs
        .iter()
        .filter(|pr| {
            ancestor_of(pr.number)
                .and_then(|a| head_to_pr.get(a))
                .is_none()
        })
        .map(|pr| pr.number)
        .collect::<Vec<u64>>();

    bottoms
        .iter()
        .map(|&bottom| {
            std::iter::successors(Some(bottom), |&current| {
                let current_head = prs.iter().find(|p| p.number == current)?.head_ref.clone();
                prs.iter()
                    .find(|p| ancestor_of(p.number).is_some_and(|a| a == &current_head))
                    .map(|p| p.number)
            })
            .collect::<Vec<u64>>()
        })
        .filter(|chain: &Vec<u64>| chain.len() >= 2)
        .collect::<Vec<Vec<u64>>>()
}

/// Propose a base ref for every PR that has a local bookmark: its stacked
/// ancestor bookmark, else `trunk`, else the base GitHub already has.
fn bases(
    prs: &[PrDetails],
    ancestors: &HashMap<u64, Option<String>>,
    head_to_local_commit: &HashMap<String, String>,
    trunk: Option<&str>,
) -> Vec<BasePlan> {
    prs.iter()
        .filter_map(|pr| {
            let local_commit = head_to_local_commit.get(&pr.head_ref)?;
            let proposed = ancestors
                .get(&pr.number)
                .and_then(Clone::clone)
                .or_else(|| trunk.map(ToString::to_string))
                .unwrap_or_else(|| pr.base_ref.clone());
            Some(BasePlan {
                pr_number: pr.number,
                pr_node_id: pr.graphql_node_id.clone(),
                title: pr.title.clone(),
                bookmark: pr.head_ref.clone(),
                local_commit_id: local_commit.clone(),
                current_base: pr.base_ref.clone(),
                proposed_base: proposed,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jj::{CommitInfo, Jj, PushedBookmark};
    use anyhow::Result;
    use std::path::{Path, PathBuf};

    struct MockJj {
        ancestors: HashMap<String, Option<String>>,
        workspace_root: PathBuf,
    }

    impl MockJj {
        fn new() -> Self {
            Self {
                ancestors: HashMap::new(),
                workspace_root: PathBuf::from("/tmp"),
            }
        }

        fn with_ancestor(mut self, rev: &str, ancestor: Option<&str>) -> Self {
            self.ancestors
                .insert(rev.to_string(), ancestor.map(ToString::to_string));
            self
        }
    }

    impl Jj for MockJj {
        async fn default_remote(&self) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn remote_names(&self) -> Result<Vec<String>> {
            unimplemented!()
        }
        async fn resolve_rev(&self, _rev: &str) -> Result<CommitInfo> {
            unimplemented!()
        }
        async fn remote_url(&self, _name: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn remote_bookmark_sha(
            &self,
            _bookmark: &str,
            _remote: &str,
        ) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn trunk_branch(&self) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn workspace_root(&self) -> Result<&PathBuf> {
            Ok(&self.workspace_root)
        }
        async fn git_import(&self) -> Result<()> {
            unimplemented!()
        }
        async fn stacked_ancestor_bookmark(&self, rev: &str) -> Result<Option<String>> {
            Ok(self.ancestors.get(rev).cloned().flatten())
        }
        async fn first_commit_description(&self, _revset: &str) -> Result<String> {
            unimplemented!()
        }
        async fn push(&self, _rev: &str, _bookmark: Option<&str>, _remote: String) -> Result<()> {
            unimplemented!()
        }
        async fn pushed_bookmarks(&self, _remote: &str) -> Result<Vec<PushedBookmark>> {
            unimplemented!()
        }
        async fn eval_template(
            &self,
            _revset: &str,
            _template: &str,
            _config_file: Option<&Path>,
            _reversed: bool,
            _color: bool,
        ) -> Result<String> {
            unimplemented!()
        }
        async fn diff(&self, _revset: &str) -> Result<String> {
            unimplemented!()
        }
        async fn pr_diff(&self, _base_oid: &str, _head_oid: &str) -> Result<String> {
            unimplemented!()
        }
    }

    fn pr(number: u64, head_ref: &str, head_sha: &str) -> PrDetails {
        pr_based_on(number, head_ref, head_sha, "main")
    }

    fn pr_based_on(number: u64, head_ref: &str, head_sha: &str, base_ref: &str) -> PrDetails {
        PrDetails {
            number,
            head_ref: head_ref.to_string(),
            head_sha: head_sha.to_string(),
            base_ref: base_ref.to_string(),
            title: format!("PR {number}"),
            html_url: format!("https://github.com/o/r/pull/{number}"),
            is_draft: false,
            auto_merge: false,
            auto_merge_method: None,
            base_sha: "base".to_string(),
            head_user_login: None,
            head_repo_name: None,
            graphql_node_id: format!("node_{number}"),
            in_merge_queue: false,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
            stack_number: None,
        }
    }

    /// Map each PR's head ref to its head sha, i.e. "nothing has been rebased
    /// locally since the last push".
    fn local_map(prs: &[PrDetails]) -> HashMap<String, String> {
        prs.iter()
            .map(|p| (p.head_ref.clone(), p.head_sha.clone()))
            .collect()
    }

    async fn chains_of(prs: &[PrDetails], jj: &MockJj) -> Vec<Vec<u64>> {
        detect(prs, jj, &local_map(prs), None).await.unwrap().chains
    }

    #[tokio::test]
    async fn no_prs_returns_empty() {
        let jj = MockJj::new();
        let shape = detect(&[], &jj, &HashMap::new(), None).await.unwrap();
        assert!(shape.chains.is_empty());
        assert!(shape.bases.is_empty());
    }

    #[tokio::test]
    async fn single_pr_forms_no_chain() {
        let jj = MockJj::new().with_ancestor("sha1", None);
        let prs = vec![pr(1, "branch-a", "sha1")];
        assert!(chains_of(&prs, &jj).await.is_empty());
    }

    #[tokio::test]
    async fn two_unrelated_prs_returns_empty() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", None);
        let prs = vec![pr(1, "branch-a", "sha1"), pr(2, "branch-b", "sha2")];
        assert!(chains_of(&prs, &jj).await.is_empty());
    }

    #[tokio::test]
    async fn two_stacked_prs_returns_chain() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", Some("branch-a"));
        let prs = vec![pr(1, "branch-a", "sha1"), pr(2, "branch-b", "sha2")];
        assert_eq!(chains_of(&prs, &jj).await, vec![vec![1, 2]]);
    }

    #[tokio::test]
    async fn three_stacked_prs_returns_chain() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", Some("branch-a"))
            .with_ancestor("sha3", Some("branch-b"));
        let prs = vec![
            pr(1, "branch-a", "sha1"),
            pr(2, "branch-b", "sha2"),
            pr(3, "branch-c", "sha3"),
        ];
        assert_eq!(chains_of(&prs, &jj).await, vec![vec![1, 2, 3]]);
    }

    #[tokio::test]
    async fn mixed_stacked_and_unrelated_returns_only_chains() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", Some("branch-a"))
            .with_ancestor("sha3", None)
            .with_ancestor("sha4", Some("branch-c"));
        let prs = vec![
            pr(1, "branch-a", "sha1"),
            pr(2, "branch-b", "sha2"),
            pr(3, "branch-c", "sha3"),
            pr(4, "branch-d", "sha4"),
        ];
        let chains = chains_of(&prs, &jj).await;
        assert_eq!(chains.len(), 2);
        assert!(chains.contains(&vec![1, 2]));
        assert!(chains.contains(&vec![3, 4]));
    }

    #[tokio::test]
    async fn ancestor_not_in_set_is_not_stacked() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", Some("branch-not-in-set"));
        let prs = vec![pr(1, "branch-a", "sha1"), pr(2, "branch-b", "sha2")];
        assert!(chains_of(&prs, &jj).await.is_empty());
    }

    #[tokio::test]
    async fn base_follows_the_stacked_ancestor_bookmark() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", Some("branch-a"));
        let prs = vec![pr(1, "branch-a", "sha1"), pr(2, "branch-b", "sha2")];
        let shape = detect(&prs, &jj, &local_map(&prs), Some("main"))
            .await
            .unwrap();

        let proposed = shape
            .bases
            .iter()
            .map(|b| (b.pr_number, b.proposed_base.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(proposed, vec![(1, "main"), (2, "branch-a")]);
    }

    #[tokio::test]
    async fn lone_pr_left_by_a_merged_bottom_retargets_to_trunk() {
        // The bottom PR merged and its bookmark is gone locally, so nothing
        // bookmarked sits below the survivor. Its base still points at the
        // merged branch; only a trunk fallback fixes that. Regression guard
        // for the case `pr stack` used to ignore because it forms no chain.
        let jj = MockJj::new().with_ancestor("sha2", None);
        let prs = vec![pr_based_on(2, "branch-b", "sha2", "branch-a")];
        let shape = detect(&prs, &jj, &local_map(&prs), Some("main"))
            .await
            .unwrap();

        assert!(shape.chains.is_empty());
        assert_eq!(shape.bases.len(), 1);
        assert_eq!(shape.bases[0].proposed_base, "main");
        assert!(!shape.bases[0].is_no_change());
    }

    #[tokio::test]
    async fn base_falls_back_to_current_when_no_trunk() {
        let jj = MockJj::new().with_ancestor("sha1", None);
        let prs = vec![pr_based_on(1, "branch-a", "sha1", "release")];
        let shape = detect(&prs, &jj, &local_map(&prs), None).await.unwrap();

        assert_eq!(shape.bases[0].proposed_base, "release");
        assert!(shape.bases[0].is_no_change());
    }

    #[tokio::test]
    async fn pr_without_a_local_bookmark_gets_no_base_plan() {
        let jj = MockJj::new();
        let prs = vec![pr(1, "branch-a", "sha1")];
        let shape = detect(&prs, &jj, &HashMap::new(), Some("main"))
            .await
            .unwrap();
        assert!(shape.bases.is_empty());
    }
}
