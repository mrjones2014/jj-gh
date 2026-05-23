//! End-to-end orchestrator for `jj-gh pr create`.

mod editor;
mod frontmatter;
mod template;
mod validation;

pub use editor::{EditorRoundTrip, TempfileEditor, resolve_editor_argv};
pub use frontmatter::Frontmatter;
pub use template::{TemplateChoice, load_template_file, resolve_template_path};

use crate::{
    auth,
    cli::{CreateArgs, PrAction},
    config::{self, Config},
    fs::RealFs,
    gh::{self, CreatePrRequest, Gh, remote},
    jj::{self, Jj},
};
use anyhow::{Context, Result, anyhow};

pub async fn dispatch(action: PrAction) -> Result<()> {
    let config = config::load()?;
    let token = auth::resolve_token(&config).await?;
    let jj = jj::real::JjCli;
    let gh = gh::real::OctocrabGh::new(&token)?;
    let editor = TempfileEditor;
    match action {
        PrAction::Create(args) => create(&jj, &gh, &editor, &config, &args).await?,
    }

    Ok(())
}

/// Run the full pr-create flow.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, push, editor, etc.).
async fn create<J, G, E>(
    jj: &J,
    gh: &G,
    editor: &E,
    config: &Config,
    args: &CreateArgs,
) -> Result<()>
where
    J: Jj,
    G: Gh,
    E: EditorRoundTrip,
{
    let info = jj.resolve_rev(&args.rev)?;
    let existing_branch = info.bookmarks.first().cloned();

    let origin_url = jj
        .remote_url("origin")?
        .ok_or_else(|| anyhow!("origin remote is not configured"))?;
    let upstream_url = jj.remote_url("upstream")?;
    let target = remote::target(&origin_url, upstream_url.as_deref())?;

    // Pre-flight only when we already have a bookmark; an unpushed rev can't have
    // a matching open PR.
    if let Some(branch) = &existing_branch {
        let head_spec = target.head_spec(branch);
        if let Some(existing) = gh
            .find_open_pr(&target.owner, &target.repo, &head_spec)
            .await?
        {
            log::info!(
                "PR #{} is already {} for `{}`: {}",
                existing.number,
                existing.state,
                head_spec,
                existing.title,
            );
            println!("{}", existing.html_url);
            return Ok(());
        }
    }

    let ancestor = jj.stacked_ancestor_bookmark(&args.rev)?;
    let detected_base = jj
        .trunk_branch()?
        .unwrap_or_else(|| config.default_base_branch.clone());
    let base = resolve_base(args, ancestor.as_deref(), &detected_base);

    if !gh.branch_exists(&target.owner, &target.repo, &base).await? {
        return Err(anyhow!(
            "base branch `{base}` does not exist on {}/{}",
            target.owner,
            target.repo,
        ));
    }

    let title_revset = jj::title_base_revset(&args.rev, ancestor.as_deref());
    let default_title = jj.first_commit_description(&title_revset)?;

    let raw_template = load_template_for(args, config, jj)?;
    let initial_fm = Frontmatter {
        title: default_title,
        base: base.clone(),
        labels: vec![],
        draft: resolve_draft(args, config),
    };
    let initial_buffer = initial_fm.render(raw_template.as_deref().unwrap_or(""))?;
    let raw_template_body = raw_template.unwrap_or_default();

    let visual = std::env::var("VISUAL").ok();
    let editor_env = std::env::var("EDITOR").ok();
    let editor_argv = resolve_editor_argv(args, config, visual.as_deref(), editor_env.as_deref())?;
    let edited = editor.edit(&editor_argv, &initial_buffer).await?;
    let (final_fm, body) = Frontmatter::parse(&edited)?;
    validation::validate(&final_fm, &body, &raw_template_body)?;

    jj.push(&args.rev).await?;

    let branch = if let Some(b) = existing_branch {
        b
    } else {
        let refreshed = jj.resolve_rev(&args.rev)?;
        refreshed
            .bookmarks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("`jj git push -c {}` did not create a bookmark", args.rev))?
    };
    let head_spec = target.head_spec(&branch);
    let final_base = final_fm.base.clone();

    let created = gh
        .create_pr(CreatePrRequest {
            owner: target.owner.clone(),
            repo: target.repo.clone(),
            title: final_fm.title,
            body,
            head: head_spec,
            base: final_base,
            draft: final_fm.draft,
        })
        .await?;

    if !final_fm.labels.is_empty() {
        gh.add_labels(
            &target.owner,
            &target.repo,
            created.number,
            &final_fm.labels,
        )
        .await
        .context("PR created, but adding labels failed")?;
    }

    println!("{}", created.html_url);
    Ok(())
}

fn resolve_base(args: &CreateArgs, ancestor: Option<&str>, detected: &str) -> String {
    args.base
        .clone()
        .or_else(|| ancestor.map(str::to_string))
        .unwrap_or_else(|| detected.to_string())
}

fn resolve_draft(args: &CreateArgs, config: &Config) -> bool {
    if args.draft {
        return true;
    }
    if args.no_draft {
        return false;
    }
    config.draft
}

fn load_template_for<J: Jj>(args: &CreateArgs, config: &Config, _jj: &J) -> Result<Option<String>> {
    let repo_root = std::env::current_dir().context("could not read cwd")?;
    let fs = RealFs;
    match resolve_template_path(args, config, &repo_root, &fs) {
        TemplateChoice::None => Ok(None),
        TemplateChoice::Path(p) => load_template_file(&p, &fs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(draft: bool, no_draft: bool) -> CreateArgs {
        CreateArgs {
            rev: "@-".into(),
            base: None,
            draft,
            no_draft,
            template: None,
            no_template: false,
            editor: None,
            gh_askpass: None,
            askpass_timeout_secs: None,
        }
    }

    fn args_with_base(base: Option<&str>) -> CreateArgs {
        let mut a = cli(false, false);
        a.base = base.map(str::to_string);
        a
    }

    #[test]
    fn base_cli_wins_over_ancestor_and_detected() {
        assert_eq!(
            resolve_base(&args_with_base(Some("release")), Some("ancestor"), "main"),
            "release"
        );
    }

    #[test]
    fn base_ancestor_wins_over_detected() {
        assert_eq!(
            resolve_base(&args_with_base(None), Some("ancestor"), "main"),
            "ancestor"
        );
    }

    #[test]
    fn base_falls_back_to_detected() {
        assert_eq!(resolve_base(&args_with_base(None), None, "main"), "main");
    }

    fn cfg_with_draft(draft: bool) -> Config {
        Config {
            draft,
            ..Config::default()
        }
    }

    #[test]
    fn draft_flag_forces_true() {
        assert!(resolve_draft(&cli(true, false), &cfg_with_draft(false)));
    }

    #[test]
    fn no_draft_flag_forces_false_even_when_config_draft() {
        assert!(!resolve_draft(&cli(false, true), &cfg_with_draft(true)));
    }

    #[test]
    fn draft_defaults_to_config() {
        assert!(resolve_draft(&cli(false, false), &cfg_with_draft(true)));
        assert!(!resolve_draft(&cli(false, false), &cfg_with_draft(false)));
    }
}
