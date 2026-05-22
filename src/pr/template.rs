//! PR-body template lookup.
//!
//! Pure path resolution lives in [`resolve_template_path`]; reading the file at
//! that path is done by [`load_template_file`]. Both delegate filesystem reads
//! to a [`FileSystem`] so tests stay hermetic.

use crate::{cli::CreateArgs, config::Config, fs::FileSystem};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Result of [`resolve_template_path`]: where to look for the template, or that
/// no template should be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateChoice {
    /// Skip templating entirely (--no-template or no candidates found).
    None,
    /// Read the template from this path.
    Path(PathBuf),
}

/// Resolve which template to use based on CLI flags, config, and the repo root.
///
/// Precedence (high to low):
/// 1. `--no-template` => `None`
/// 2. `--template <name-or-path>`
/// 3. `template_path` in config
/// 4. Auto-detect `<repo>/.github/PULL_REQUEST_TEMPLATE.md`
#[must_use]
pub fn resolve_template_path<F: FileSystem>(
    args: &CreateArgs,
    config: &Config,
    repo_root: &Path,
    fs: &F,
) -> TemplateChoice {
    if args.no_template {
        return TemplateChoice::None;
    }

    if let Some(name) = args.template.as_deref() {
        return TemplateChoice::Path(resolve_cli_template(name, repo_root));
    }

    if let Some(p) = config.template_path.as_deref() {
        return TemplateChoice::Path(p.to_path_buf());
    }

    for candidate in [
        ".github/PULL_REQUEST_TEMPLATE.md",
        ".github/PULL_REQUEST_TEMPLATE/PULL_REQUEST_TEMPLATE.md",
        ".github/pull_request_template.md",
        ".github/PULL_REQUEST_TEMPLATE/pull_request_template.md",
    ] {
        let path = repo_root.join(candidate);
        if fs.exists(&path) {
            return TemplateChoice::Path(path);
        }
    }

    TemplateChoice::None
}

fn resolve_cli_template(name: &str, repo_root: &Path) -> PathBuf {
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

    fn args(rev: &str) -> CreateArgs {
        CreateArgs {
            rev: rev.into(),
            base: None,
            draft: false,
            no_draft: false,
            template: None,
            no_template: false,
            editor: None,
            gh_askpass: None,
            askpass_timeout_secs: None,
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn no_template_flag_overrides_everything() {
        let mut a = args("@-");
        a.no_template = true;
        a.template = Some("custom.md".into());
        let mut c = cfg();
        c.template_path = Some(PathBuf::from("/cfg.md"));
        let choice = resolve_template_path(&a, &c, Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(choice, TemplateChoice::None);
    }

    #[test]
    fn cli_name_resolves_under_pull_request_template_dir() {
        let mut a = args("@-");
        a.template = Some("bug".into());
        let choice = resolve_template_path(&a, &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(
            choice,
            TemplateChoice::Path(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE/bug.md"))
        );
    }

    #[test]
    fn cli_name_keeps_md_extension_if_already_present() {
        let mut a = args("@-");
        a.template = Some("bug.md".into());
        let choice = resolve_template_path(&a, &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(
            choice,
            TemplateChoice::Path(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE/bug.md"))
        );
    }

    #[test]
    fn cli_path_with_dot_is_used_verbatim() {
        let mut a = args("@-");
        a.template = Some("./local.md".into());
        let choice = resolve_template_path(&a, &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(choice, TemplateChoice::Path(PathBuf::from("./local.md")));
    }

    #[test]
    fn cli_dotfile_name_still_resolves_under_pull_request_template_dir() {
        let mut a = args("@-");
        a.template = Some(".secret".into());
        let choice = resolve_template_path(&a, &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(
            choice,
            TemplateChoice::Path(PathBuf::from(
                "/repo/.github/PULL_REQUEST_TEMPLATE/.secret.md"
            ))
        );
    }

    #[test]
    fn cli_parent_relative_path_is_used_verbatim() {
        let mut a = args("@-");
        a.template = Some("../shared.md".into());
        let choice = resolve_template_path(&a, &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(choice, TemplateChoice::Path(PathBuf::from("../shared.md")));
    }

    #[test]
    fn cli_absolute_path_is_used_verbatim() {
        let mut a = args("@-");
        a.template = Some("/etc/template.md".into());
        let choice = resolve_template_path(&a, &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(
            choice,
            TemplateChoice::Path(PathBuf::from("/etc/template.md"))
        );
    }

    #[test]
    fn config_used_when_cli_absent() {
        let mut c = cfg();
        c.template_path = Some(PathBuf::from("/cfg.md"));
        let choice = resolve_template_path(&args("@-"), &c, Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(choice, TemplateChoice::Path(PathBuf::from("/cfg.md")));
    }

    #[test]
    fn auto_detects_uppercase_github_template() {
        let fs = FakeFs::new(&[("/repo/.github/PULL_REQUEST_TEMPLATE.md", "")]);
        let choice = resolve_template_path(&args("@-"), &cfg(), Path::new("/repo"), &fs);
        assert_eq!(
            choice,
            TemplateChoice::Path(PathBuf::from("/repo/.github/PULL_REQUEST_TEMPLATE.md"))
        );
    }

    #[test]
    fn auto_detects_lowercase_fallback() {
        let fs = FakeFs::new(&[("/repo/.github/pull_request_template.md", "")]);
        let choice = resolve_template_path(&args("@-"), &cfg(), Path::new("/repo"), &fs);
        assert_eq!(
            choice,
            TemplateChoice::Path(PathBuf::from("/repo/.github/pull_request_template.md"))
        );
    }

    #[test]
    fn none_when_no_template_found() {
        let choice =
            resolve_template_path(&args("@-"), &cfg(), Path::new("/repo"), &FakeFs::new(&[]));
        assert_eq!(choice, TemplateChoice::None);
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
