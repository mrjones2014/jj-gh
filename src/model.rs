//! High-level application model shared by command handlers.
//!
//! Commands depend only on [`Model`]. Low-level traits remain available behind
//! the model for focused helpers and test fakes, while cross-system workflows
//! have one canonical home.

use crate::{
    auth::{EnvReader, OsEnv},
    editor::{Editor, TempfileEditor},
    gh::{
        Gh, PrDetails, PrWithCiStatus,
        pr_lookup::{self, PrLookup},
        real::OctocrabGh,
        remote::{self, Target},
    },
    git::real::{GitOps, RealGit},
    jj::{Jj, JjExt, PushedBookmark, real::JjCli},
};
use anyhow::{Result, anyhow};
use secrecy::SecretString;
use std::{path::PathBuf, rc::Rc};

pub trait Model {
    type Editor: Editor;
    type Env: EnvReader;
    type Gh: Gh;
    type Git: GitOps;
    type Jj: Jj;

    fn editor(&self) -> &Self::Editor;
    fn env(&self) -> &Self::Env;
    fn gh(&self) -> &Self::Gh;
    fn git(&self) -> &Self::Git;
    fn jj(&self) -> &Self::Jj;

    /// Resolve the local push remote and PR target, then return open PRs whose
    /// head belongs to that push remote owner and matches a tracked bookmark.
    async fn local_pulls(
        &self,
        remote: Option<&String>,
        upstream_remote: Option<&str>,
    ) -> Result<LocalPulls> {
        let (remote, target) = self.resolve_target(remote, upstream_remote).await?;
        let bookmarks = self.jj().pushed_bookmarks(&remote).await?;
        let names = bookmarks
            .iter()
            .map(|bookmark| bookmark.name.clone())
            .collect::<Vec<_>>();
        let prs = self
            .gh()
            .local_pulls(&target.owner, &target.repo, target.origin_owner(), &names)
            .await?;
        Ok(LocalPulls {
            target,
            bookmarks,
            prs,
        })
    }

    async fn resolve_pr(
        &self,
        remote: Option<&String>,
        upstream_remote: &str,
        number_or_rev: &str,
    ) -> Result<PrDetails> {
        let remote = self.jj().resolve_default_remote(remote).await?;
        pr_lookup::get_pr(
            self.jj(),
            self.gh(),
            &remote,
            upstream_remote,
            number_or_rev,
        )
        .await
    }

    async fn resolve_pr_number_with_target(
        &self,
        remote: Option<&String>,
        upstream_remote: &str,
        number_or_rev: &str,
    ) -> Result<(Target, u64)> {
        let (remote, target) = self.resolve_target(remote, Some(upstream_remote)).await?;
        if let Ok(number) = number_or_rev.parse::<u64>() {
            return Ok((target, number));
        }
        let PrLookup {
            target,
            head_spec,
            summary,
            ..
        } = pr_lookup::resolve_pr_for_rev(
            self.jj(),
            self.gh(),
            &remote,
            upstream_remote,
            number_or_rev,
        )
        .await?;
        let summary = summary.ok_or_else(|| {
            anyhow!("no open PR for revision `{number_or_rev}` (head `{head_spec}`)")
        })?;
        Ok((target, summary.number))
    }

    async fn resolve_pr_with_target(
        &self,
        remote: Option<&String>,
        upstream_remote: &str,
        number_or_rev: &str,
    ) -> Result<(PrDetails, Target)> {
        let remote = self.jj().resolve_default_remote(remote).await?;
        pr_lookup::resolve_pr_with_target(
            self.jj(),
            self.gh(),
            &remote,
            upstream_remote,
            number_or_rev,
        )
        .await
    }

    async fn resolve_target(
        &self,
        remote: Option<&String>,
        upstream_remote: Option<&str>,
    ) -> Result<(String, Target)> {
        let remote = self.jj().resolve_default_remote(remote).await?;
        let origin_url = self
            .jj()
            .remote_url(&remote)
            .await?
            .ok_or_else(|| anyhow!("`{remote}` remote is not configured"))?;
        let upstream_url = match upstream_remote {
            Some(name) => self.jj().remote_url(name).await?,
            None => None,
        };
        let target = remote::target(&origin_url, upstream_url.as_deref())?;
        Ok((remote, target))
    }
}

pub struct ModelImpl {
    editor: TempfileEditor,
    env: OsEnv,
    gh: OctocrabGh,
    git: RealGit,
    jj: JjCli,
}

impl ModelImpl {
    pub fn new(
        repo: gix::Repository,
        workspace_root: PathBuf,
        token: &SecretString,
    ) -> Result<Self> {
        let repo = Rc::new(repo);
        Ok(Self {
            editor: TempfileEditor,
            env: OsEnv,
            gh: OctocrabGh::new(token)?,
            git: RealGit::new(Rc::clone(&repo)),
            jj: JjCli::from_repository(repo, workspace_root),
        })
    }
}

impl Model for ModelImpl {
    type Editor = TempfileEditor;
    type Env = OsEnv;
    type Gh = OctocrabGh;
    type Git = RealGit;
    type Jj = JjCli;

    fn editor(&self) -> &Self::Editor {
        &self.editor
    }

    fn env(&self) -> &Self::Env {
        &self.env
    }

    fn gh(&self) -> &Self::Gh {
        &self.gh
    }

    fn git(&self) -> &Self::Git {
        &self.git
    }

    fn jj(&self) -> &Self::Jj {
        &self.jj
    }
}

pub struct LocalPulls {
    pub target: remote::Target,
    pub bookmarks: Vec<PushedBookmark>,
    pub prs: Vec<PrWithCiStatus>,
}

#[cfg(test)]
pub(crate) struct TestModel<'a, J, G, GO = NoGit> {
    editor: TempfileEditor,
    env: OsEnv,
    gh: &'a G,
    git: &'a GO,
    jj: &'a J,
}

#[cfg(test)]
impl<'a, J, G, GO> TestModel<'a, J, G, GO> {
    pub(crate) fn new(jj: &'a J, gh: &'a G, git: &'a GO) -> Self {
        Self {
            editor: TempfileEditor,
            env: OsEnv,
            gh,
            git,
            jj,
        }
    }
}

#[cfg(test)]
impl<'a, J, G> TestModel<'a, J, G> {
    pub(crate) fn without_git(jj: &'a J, gh: &'a G) -> Self {
        Self::new(jj, gh, &NoGit)
    }
}

#[cfg(test)]
impl<J: Jj, G: Gh, GO: GitOps> Model for TestModel<'_, J, G, GO> {
    type Editor = TempfileEditor;
    type Env = OsEnv;
    type Gh = G;
    type Git = GO;
    type Jj = J;

    fn editor(&self) -> &Self::Editor {
        &self.editor
    }

    fn env(&self) -> &Self::Env {
        &self.env
    }

    fn gh(&self) -> &Self::Gh {
        self.gh
    }

    fn git(&self) -> &Self::Git {
        self.git
    }

    fn jj(&self) -> &Self::Jj {
        self.jj
    }
}

#[cfg(test)]
pub(crate) struct NoGit;

#[cfg(test)]
impl GitOps for NoGit {
    async fn local_bookmark_exists(&self, _name: &str) -> Result<bool> {
        unreachable!("test did not configure git operations")
    }

    async fn fetch_pr(&self, _remote: &str, _pr: u64, _bookmark: &str, _force: bool) -> Result<()> {
        unreachable!("test did not configure git operations")
    }
}
