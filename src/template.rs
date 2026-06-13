//! PR-body template lookup.
//!
//! [`resolve_template_source`] picks between a jj-template string, a file
//! path, or nothing based on CLI flags, layered jj config, the repo root, and
//! the filesystem. [`load_template_file`] reads a chosen file via a
//! [`FileSystem`] so tests stay hermetic.

use crate::{commands::pr::CreateArgs, config::LayerTemplate, fs::FileSystem};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Where the PR body template comes from. `JjTemplate` is a jj template
/// string the caller evaluates via `jj log -T`; `File` is a markdown file the
/// caller reads off disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    /// Skip templating entirely (`--no-template` or no candidates found).
    None,
    /// Evaluate this jj template string for the body.
    JjTemplate(String),
    /// Read the body from this path.
    File(PathBuf),
}

/// Resolve the PR body template from CLI args, layered jj config, and the
/// filesystem.
///
/// Precedence, highest first:
/// 1. `--no-template` flag.
/// 2. `-T|--template` CLI (jj template string).
/// 3. `--template-file` CLI (path under `.github/PULL_REQUEST_TEMPLATE/` or
///    verbatim if absolute / `~`-prefixed / starts with `./` or `../`).
/// 4. Repo-layer `pr_create_template` (jj template string from `--repo`,
///    `--workspace`, or `JJ_GH_EXTRA_CONFIG`).
/// 5. Repo-layer `pr_create_template_file` (path).
/// 6. Auto-detect `<repo>/.github/PULL_REQUEST_TEMPLATE.md` (and case
///    variants).
/// 7. User-layer `pr_create_template` (jj template string from `--user`).
/// 8. User-layer `pr_create_template_file` (path).
///
/// The split between repo and user layers lets a contributor set a global
/// default jj template while still picking up per-repo
/// `.github/PULL_REQUEST_TEMPLATE.md` files when contributing to OSS.
#[must_use]
pub fn resolve_template_source<F: FileSystem>(
    args: &CreateArgs,
    repo_layer: &LayerTemplate,
    user_layer: &LayerTemplate,
    repo_root: &Path,
    fs: &F,
) -> TemplateSource {
    if args.no_template {
        return TemplateSource::None;
    }

    if let Some(t) = args.template.as_deref() {
        return TemplateSource::JjTemplate(t.to_string());
    }

    if let Some(name) = args.template_file.as_deref() {
        return TemplateSource::File(resolve_cli_template_file(name, repo_root));
    }

    if let Some(t) = repo_layer.pr_create_template.as_deref() {
        return TemplateSource::JjTemplate(t.to_string());
    }

    if let Some(p) = repo_layer.pr_create_template_file.as_deref() {
        return TemplateSource::File(p.to_path_buf());
    }

    for candidate in [
        ".github/PULL_REQUEST_TEMPLATE.md",
        ".github/PULL_REQUEST_TEMPLATE/PULL_REQUEST_TEMPLATE.md",
        ".github/pull_request_template.md",
        ".github/PULL_REQUEST_TEMPLATE/pull_request_template.md",
    ] {
        let path = repo_root.join(candidate);
        if fs.exists(&path) {
            return TemplateSource::File(path);
        }
    }

    if let Some(t) = user_layer.pr_create_template.as_deref() {
        return TemplateSource::JjTemplate(t.to_string());
    }

    if let Some(p) = user_layer.pr_create_template_file.as_deref() {
        return TemplateSource::File(p.to_path_buf());
    }

    TemplateSource::None
}

fn resolve_cli_template_file(name: &str, repo_root: &Path) -> PathBuf {
    if name.starts_with("./")
        || name.starts_with("../")
        || name.starts_with('/')
        || name.starts_with('~')
    {
        return PathBuf::from(name);
    }

    let with_ext = if Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
    {
        name.to_string()
    } else {
        format!("{name}.md")
    };

    repo_root
        .join(".github")
        .join("PULL_REQUEST_TEMPLATE")
        .join(with_ext)
}

/// Read the template file via `fs`, returning trimmed contents or `None` if absent.
///
/// # Errors
///
/// Propagates IO errors other than "not found".
pub fn load_template_file<F: FileSystem>(path: &Path, fs: &F) -> Result<Option<String>> {
    Ok(fs.read_to_string(path)?.map(|s| s.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FakeFs;

    fn args() -> CreateArgs {
        CreateArgs {
            rev: "@-".into(),
            base: crate::util::EvalWithCfgFallback::new(None, None),
            draft: false,
            no_draft: false,
            auto_merge: false,
            no_auto_merge: false,
            auto_merge_method: crate::config::AutoMergeMethod::Merge,
            template: None,
            template_file: None,
            no_template: false,
            pick_title: false,
            title_template: "description.first_line()".into(),
            editor: None,
            show_diffs: true,
            no_diffs: false,
            globals: crate::cli::GlobalOpts {
                verbose: 0,
                quiet: false,
                log_level: None,
                remote: Some("origin".into()),
                upstream_remote: "upstream".into(),
                gh_askpass: None,
                askpass_timeout_secs: 20,
            },
        }
    }

    fn empty_layer() -> LayerTemplate {
        LayerTemplate::default()
    }

    #[test]
    fn no_template_flag_overrides_everything() {
        let mut a = args();
        a.no_template = true;
        a.template = Some("description".into());
        a.template_file = Some("custom.md".into());
        let mut repo = empty_layer();
        repo.pr_create_template = Some("description".into());
        repo.pr_create_template_file = Some(PathBuf::from("/cfg.md"));
        let mut user = empty_layer();
        user.pr_create_template = Some("description".into());

        let choice =
            resolve_template_source(&a, &repo, &user, Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(choice, TemplateSource::None);
    }

    #[test]
    fn cli_jj_template_string_wins_over_cli_file() {
        let mut a = args();
        a.template = Some("description".into());
        a.template_file = Some("bug.md".into());

        let choice = resolve_template_source(
            &a,
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(choice, TemplateSource::JjTemplate("description".into()));
    }

    #[test]
    fn cli_template_file_resolves_under_pull_request_template_dir() {
        let mut a = args();
        a.template_file = Some("bug".into());
        let choice = resolve_template_source(
            &a,
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(
            choice,
            TemplateSource::File(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE/bug.md"))
        );
    }

    #[test]
    fn cli_template_file_keeps_md_extension_if_already_present() {
        let mut a = args();
        a.template_file = Some("bug.md".into());
        let choice = resolve_template_source(
            &a,
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(
            choice,
            TemplateSource::File(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE/bug.md"))
        );
    }

    #[test]
    fn cli_template_file_dot_path_used_verbatim() {
        let mut a = args();
        a.template_file = Some("./local.md".into());
        let choice = resolve_template_source(
            &a,
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(choice, TemplateSource::File(PathBuf::from("./local.md")));
    }

    #[test]
    fn cli_template_file_absolute_path_used_verbatim() {
        let mut a = args();
        a.template_file = Some("/etc/template.md".into());
        let choice = resolve_template_source(
            &a,
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(
            choice,
            TemplateSource::File(PathBuf::from("/etc/template.md"))
        );
    }

    #[test]
    fn repo_layer_jj_template_used_when_cli_absent() {
        let mut repo = empty_layer();
        repo.pr_create_template = Some("description".into());
        let choice = resolve_template_source(
            &args(),
            &repo,
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(choice, TemplateSource::JjTemplate("description".into()));
    }

    #[test]
    fn repo_layer_jj_template_wins_over_github_file_autodetect() {
        let mut repo = empty_layer();
        repo.pr_create_template = Some("description".into());
        let fs = FakeFs::new(&[("/repo/.github/PULL_REQUEST_TEMPLATE.md", "body")]);
        let choice =
            resolve_template_source(&args(), &repo, &empty_layer(), Path::new("/repo"), &fs);
        assert_eq!(choice, TemplateSource::JjTemplate("description".into()));
    }

    #[test]
    fn repo_layer_file_used_when_cli_absent() {
        let mut repo = empty_layer();
        repo.pr_create_template_file = Some(PathBuf::from("/cfg.md"));
        let choice = resolve_template_source(
            &args(),
            &repo,
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(choice, TemplateSource::File(PathBuf::from("/cfg.md")));
    }

    #[test]
    fn github_autodetect_wins_over_user_layer() {
        let mut user = empty_layer();
        user.pr_create_template = Some("from-user".into());
        let fs = FakeFs::new(&[("/repo/.github/PULL_REQUEST_TEMPLATE.md", "body")]);
        let choice =
            resolve_template_source(&args(), &empty_layer(), &user, Path::new("/repo"), &fs);
        assert_eq!(
            choice,
            TemplateSource::File(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE.md"))
        );
    }

    #[test]
    fn user_layer_jj_template_used_when_no_repo_or_github_template() {
        let mut user = empty_layer();
        user.pr_create_template = Some("from-user".into());
        let choice = resolve_template_source(
            &args(),
            &empty_layer(),
            &user,
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(choice, TemplateSource::JjTemplate("from-user".into()));
    }

    #[test]
    fn auto_detects_uppercase_github_template() {
        let fs = FakeFs::new(&[("/repo/.github/PULL_REQUEST_TEMPLATE.md", "")]);
        let choice = resolve_template_source(
            &args(),
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &fs,
        );
        assert_eq!(
            choice,
            TemplateSource::File(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE.md"))
        );
    }

    #[test]
    fn auto_detects_lowercase_fallback() {
        let fs = FakeFs::new(&[("/repo/.github/pull_request_template.md", "")]);
        let choice = resolve_template_source(
            &args(),
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &fs,
        );
        assert_eq!(
            choice,
            TemplateSource::File(PathBuf::from("/repo/.github/pull_request_template.md"))
        );
    }

    #[test]
    fn none_when_no_template_found() {
        let choice = resolve_template_source(
            &args(),
            &empty_layer(),
            &empty_layer(),
            Path::new("/repo"),
            &FakeFs::new(&[]),
        );
        assert_eq!(choice, TemplateSource::None);
    }

    #[test]
    fn load_returns_trimmed_contents() {
        let fs = FakeFs::new(&[("/x.md", "  body  \n\n")]);
        let body = load_template_file(Path::new("/x.md"), &fs).unwrap();
        assert_eq!(body, Some("body".into()));
    }

    #[test]
    fn load_returns_none_when_absent() {
        let fs = FakeFs::new(&[]);
        let body = load_template_file(Path::new("/x.md"), &fs).unwrap();
        assert!(body.is_none());
    }
}
