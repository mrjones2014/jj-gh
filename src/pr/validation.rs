//! Validation of the parsed editor buffer.

use super::frontmatter::Frontmatter;
use anyhow::{Result, anyhow};

const TITLE_WARN_LEN: usize = 72;

/// Validate the parsed frontmatter + body before sending to the GH API.
///
/// `raw_template_body` is the unedited body we wrote to the tempfile; if the
/// user saved without changes, we refuse to open a PR.
///
/// # Errors
///
/// Returns an error if the title is empty, the base is empty, the body is
/// empty, or the body is unchanged from the raw template.
pub fn validate(fm: &Frontmatter, body: &str, raw_template_body: &str) -> Result<()> {
    if fm.title.trim().is_empty() {
        return Err(anyhow!("title is empty"));
    }

    if fm.base.trim().is_empty() {
        return Err(anyhow!("base is empty"));
    }

    if body.trim().is_empty() {
        return Err(anyhow!("body is empty"));
    }

    if body.trim() == raw_template_body.trim() {
        return Err(anyhow!("body is unchanged from the template"));
    }

    if fm.title.chars().count() > TITLE_WARN_LEN {
        log::warn!(
            "PR title is longer than recommended (recommend 72 max, saw {} characters)",
            fm.title.chars().count()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(title: &str) -> Frontmatter {
        Frontmatter {
            title: title.into(),
            base: "main".into(),
            labels: vec![],
            draft: false,
            auto_merge: false,
            auto_merge_method: crate::config::AutoMergeMethod::Merge,
        }
    }

    fn fm_with_base(title: &str, base: &str) -> Frontmatter {
        Frontmatter {
            title: title.into(),
            base: base.into(),
            labels: vec![],
            draft: false,
            auto_merge: false,
            auto_merge_method: crate::config::AutoMergeMethod::Merge,
        }
    }

    #[test]
    fn happy_path() {
        validate(&fm("title"), "body text", "original template").unwrap();
    }

    #[test]
    fn empty_title_errors() {
        let err = validate(&fm("   "), "body", "template").unwrap_err();
        assert!(err.to_string().contains("title is empty"));
    }

    #[test]
    fn empty_base_errors() {
        let err = validate(&fm_with_base("title", "  "), "body", "template").unwrap_err();
        assert!(err.to_string().contains("base is empty"));
    }

    #[test]
    fn empty_body_errors() {
        let err = validate(&fm("title"), "  \n", "template").unwrap_err();
        assert!(err.to_string().contains("body is empty"));
    }

    #[test]
    fn unchanged_body_errors() {
        let template = "  template text  ";
        let body = "template text";
        let err = validate(&fm("title"), body, template).unwrap_err();
        assert!(err.to_string().contains("unchanged"));
    }

    #[test]
    fn long_title_warns_but_passes() {
        let long_title = "x".repeat(TITLE_WARN_LEN + 1);
        validate(&fm(&long_title), "body", "template").unwrap();
    }
}
