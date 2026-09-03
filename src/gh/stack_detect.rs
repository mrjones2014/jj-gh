//! Stack detection logic for identifying which PRs should be linked together.

use crate::{gh::PrDetails, jj::Jj};
use anyhow::Result;
use std::collections::HashMap;

/// Detects stack chains among a set of PRs.
///
/// Returns a list of chains, where each chain is a list of PR numbers
/// ordered from bottom (closest to trunk) to top (furthest from trunk).
/// A chain has length >= 2 (single PRs are not returned).
///
/// # Arguments
/// * `prs` - The PRs to analyze
/// * `jj` - The jj interface for querying local commit information
/// * `head_to_local_commit` - Mapping from PR head branch names to local jj commit IDs
pub async fn detect_stack_chains(
    prs: &[PrDetails],
    jj: &impl Jj,
    head_to_local_commit: &HashMap<String, String>,
) -> Result<Vec<Vec<u64>>> {
    if prs.len() < 2 {
        return Ok(Vec::new());
    }

    // Build a map: head_ref -> PR number
    let head_to_pr = prs
        .iter()
        .map(|pr| (pr.head_ref.clone(), pr.number))
        .collect::<HashMap<String, u64>>();

    // For each PR, find its stacked ancestor bookmark using local commit ID
    let mut pr_to_ancestor = HashMap::<u64, Option<String>>::new();
    for pr in prs {
        let local_commit = head_to_local_commit.get(&pr.head_ref);
        let ancestor = if let Some(commit) = local_commit {
            jj.stacked_ancestor_bookmark(commit).await?
        } else {
            None
        };
        pr_to_ancestor.insert(pr.number, ancestor);
    }

    // Find bottom PRs (those whose ancestor is not another PR in the set)
    let bottoms = prs
        .iter()
        .filter(|pr| {
            let ancestor = pr_to_ancestor.get(&pr.number).and_then(|a| a.as_ref());
            ancestor.and_then(|a| head_to_pr.get(a)).is_none()
        })
        .map(|pr| pr.number)
        .collect::<Vec<u64>>();

    // Build chains from each bottom using successors
    let chains = bottoms
        .iter()
        .map(|&bottom| {
            std::iter::successors(Some(bottom), |&current| {
                let current_head = prs.iter().find(|p| p.number == current)?.head_ref.clone();
                prs.iter()
                    .find(|p| {
                        pr_to_ancestor
                            .get(&p.number)
                            .and_then(|a| a.as_ref())
                            .is_some_and(|a| a == &current_head)
                    })
                    .map(|p| p.number)
            })
            .collect::<Vec<u64>>()
        })
        .filter(|chain: &Vec<u64>| chain.len() >= 2)
        .collect::<Vec<Vec<u64>>>();

    Ok(chains)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gh::PrDetails,
        jj::{CommitInfo, Jj, PushedBookmark},
    };
    use anyhow::Result;
    use std::collections::HashMap;
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
        PrDetails {
            number,
            head_ref: head_ref.to_string(),
            head_sha: head_sha.to_string(),
            base_ref: "main".to_string(),
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

    #[tokio::test]
    async fn no_prs_returns_empty() {
        let jj = MockJj::new();
        let head_to_local = HashMap::<String, String>::new();
        let chains = detect_stack_chains(&[], &jj, &head_to_local).await.unwrap();
        assert!(chains.is_empty());
    }

    #[tokio::test]
    async fn single_pr_returns_empty() {
        let jj = MockJj::new();
        let prs = vec![pr(1, "branch-a", "sha1")];
        let mut head_to_local = HashMap::new();
        head_to_local.insert("branch-a".to_string(), "sha1".to_string());
        let chains = detect_stack_chains(&prs, &jj, &head_to_local)
            .await
            .unwrap();
        assert!(chains.is_empty());
    }

    #[tokio::test]
    async fn two_unrelated_prs_returns_empty() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", None);
        let prs = vec![pr(1, "branch-a", "sha1"), pr(2, "branch-b", "sha2")];
        let mut head_to_local = HashMap::new();
        head_to_local.insert("branch-a".to_string(), "sha1".to_string());
        head_to_local.insert("branch-b".to_string(), "sha2".to_string());
        let chains = detect_stack_chains(&prs, &jj, &head_to_local)
            .await
            .unwrap();
        assert!(chains.is_empty());
    }

    #[tokio::test]
    async fn two_stacked_prs_returns_chain() {
        let jj = MockJj::new()
            .with_ancestor("sha1", None)
            .with_ancestor("sha2", Some("branch-a"));
        let prs = vec![pr(1, "branch-a", "sha1"), pr(2, "branch-b", "sha2")];
        let mut head_to_local = HashMap::new();
        head_to_local.insert("branch-a".to_string(), "sha1".to_string());
        head_to_local.insert("branch-b".to_string(), "sha2".to_string());
        let chains = detect_stack_chains(&prs, &jj, &head_to_local)
            .await
            .unwrap();
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0], vec![1, 2]);
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
        let mut head_to_local = HashMap::new();
        head_to_local.insert("branch-a".to_string(), "sha1".to_string());
        head_to_local.insert("branch-b".to_string(), "sha2".to_string());
        head_to_local.insert("branch-c".to_string(), "sha3".to_string());
        let chains = detect_stack_chains(&prs, &jj, &head_to_local)
            .await
            .unwrap();
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0], vec![1, 2, 3]);
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
        let mut head_to_local = HashMap::new();
        head_to_local.insert("branch-a".to_string(), "sha1".to_string());
        head_to_local.insert("branch-b".to_string(), "sha2".to_string());
        head_to_local.insert("branch-c".to_string(), "sha3".to_string());
        head_to_local.insert("branch-d".to_string(), "sha4".to_string());
        let chains = detect_stack_chains(&prs, &jj, &head_to_local)
            .await
            .unwrap();
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
        let mut head_to_local = HashMap::new();
        head_to_local.insert("branch-a".to_string(), "sha1".to_string());
        head_to_local.insert("branch-b".to_string(), "sha2".to_string());
        let chains = detect_stack_chains(&prs, &jj, &head_to_local)
            .await
            .unwrap();
        assert!(chains.is_empty());
    }
}
