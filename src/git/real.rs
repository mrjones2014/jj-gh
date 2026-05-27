use anyhow::Result;

/// Operations against the workspace's git store. Abstracted so tests can
/// supply a fake.
pub trait GitOps {
    /// Whether `refs/heads/<name>` resolves in the workspace's git store.
    ///
    /// # Errors
    ///
    /// Propagates gix failures.
    async fn local_bookmark_exists(&self, name: &str) -> Result<bool>;

    /// Fetch `refs/pull/<pr>/head` from `origin` into `refs/heads/<bookmark>`.
    ///
    /// # Errors
    ///
    /// Propagates gix failures.
    async fn fetch_pr(&self, pr: u64, bookmark: &str, force: bool) -> Result<()>;
}

/// Production [`GitOps`] backed by a shared `gix::Repository` discovered
/// once at the workspace root.
pub struct RealGit {
    repo: gix::Repository,
}

impl RealGit {
    #[must_use]
    pub fn new(repo: gix::Repository) -> Self {
        Self { repo }
    }
}

impl GitOps for RealGit {
    async fn local_bookmark_exists(&self, name: &str) -> Result<bool> {
        let full = format!("refs/heads/{name}");
        Ok(self.repo.try_find_reference(full.as_str())?.is_some())
    }

    async fn fetch_pr(&self, pr: u64, bookmark: &str, force: bool) -> Result<()> {
        let prefix = if force { "+" } else { "" };
        let refspec = format!("{prefix}refs/pull/{pr}/head:refs/heads/{bookmark}");
        let remote = self
            .repo
            .find_remote("origin")?
            .with_refspecs([refspec.as_bytes()], gix::remote::Direction::Fetch)?;
        let conn = remote.connect(gix::remote::Direction::Fetch)?;
        let prep = conn.prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )?;
        let should_interrupt = std::sync::atomic::AtomicBool::new(false);
        prep.receive(gix::progress::Discard, &should_interrupt)?;
        Ok(())
    }
}
