//! YAML frontmatter for the PR editor buffer.
//!
//! Layout:
//! ````text
//! ---
//! title: ...
//! base: main
//! labels: [...]
//! draft: false
//! ---
//!
//! <body>
//!
//! # 8< jj-gh: below this line removed on submit >8
//!
//! ```diff
//! - line removed
//! + line added
//! ```
//! ````

use crate::{config::AutoMergeMethod, gh::Reviewer};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// Sentinel heading that marks the start of the read-only diff preview in the
/// `pr create` editor buffer. The marker line and everything after it are
/// stripped on parse. Whole-line exact match (after trimming trailing
/// whitespace).
pub const PREVIEW_MARKER: &str = "# 8< jj-gh: below this line removed on submit >8";

const TITLE_WARN_LEN: usize = 72;

fn is_default<T: Default + PartialEq>(val: &T) -> bool {
    *val == T::default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    pub title: String,
    pub base: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub reviewers: Vec<Reviewer>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub auto_merge_method: AutoMergeMethod,
}

impl Frontmatter {
    /// Emit YAML block + blank line + body for the editor buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML serializer fails (unlikely for our fields).
    pub fn render(&self, body: &str, preview: Option<&str>) -> Result<String> {
        let yaml = noyalib::to_string(self).context("could not serialize frontmatter")?;
        let orig_body_empty = body.trim().is_empty();
        let body = body.trim_start_matches('\n');
        let body = if body.is_empty() { "\n" } else { body };
        let body = format!("---\n{yaml}\n---\n\n{body}");
        let Some(diff) = preview else {
            return Ok(body);
        };

        // apply some extra `\n` for comfort in the editor
        if orig_body_empty {
            Ok(format!(
                "{body}\n\n\n\n{PREVIEW_MARKER}\n\n```diff\n{diff}\n```\n"
            ))
        } else {
            let trimmed = body.trim_end();
            Ok(format!(
                "{trimmed}\n\n\n\n\n{PREVIEW_MARKER}\n\n```diff\n{diff}\n```\n"
            ))
        }
    }

    /// Parse a frontmatter-prefixed markdown buffer back into `(meta, body)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the document is missing or has an unterminated
    /// frontmatter block, or if the YAML fails to deserialize.
    pub fn parse(buffer: &str) -> Result<(Self, String)> {
        let trimmed = strip_preview(buffer);
        let rest = trimmed
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow!("missing leading `---` frontmatter delimiter"))?;
        let (yaml, body) = rest
            .split_once("\n---\n")
            .or_else(|| rest.split_once("\n---"))
            .ok_or_else(|| anyhow!("unterminated frontmatter; expected closing `---`"))?;
        let fm =
            noyalib::from_str::<Frontmatter>(yaml).context("could not parse YAML frontmatter")?;
        let body = body.trim_start_matches('\n').trim_end().to_string();
        Ok((fm, body))
    }

    /// Validate this frontmatter before sending it to the GH API.
    ///
    /// # Errors
    ///
    /// Returns an error if the title or base is empty.
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            return Err(anyhow!("title is empty"));
        }

        if self.base.trim().is_empty() {
            return Err(anyhow!("base is empty"));
        }

        if self.title.chars().count() > TITLE_WARN_LEN {
            log::warn!(
                "PR title is longer than recommended (recommend 72 max, saw {} characters)",
                self.title.chars().count()
            );
        }

        Ok(())
    }
}

/// Truncate `buffer` at the first line equal to [`PREVIEW_MARKER`] (after
/// trimming trailing whitespace). Returns the original buffer if absent.
fn strip_preview(buffer: &str) -> &str {
    let user_body_len = buffer
        .split_inclusive('\n')
        .take_while(|line| line.trim() != PREVIEW_MARKER)
        .map(str::len)
        .sum();
    &buffer[..user_body_len]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{self, Editor};
    use std::sync::Mutex;

    fn fm(title: &str) -> Frontmatter {
        Frontmatter {
            title: title.into(),
            base: "main".into(),
            labels: vec!["bug".into(), "p1".into()],
            reviewers: vec![Reviewer::parse("@john-carmack").unwrap()],
            draft: false,
            auto_merge: false,
            auto_merge_method: AutoMergeMethod::Merge,
        }
    }

    #[test]
    fn render_round_trips_through_parse() {
        let original = fm("Fix the thing");
        let body = "This PR does X and Y.\n";
        let rendered = original.render(body, None).unwrap();

        let (parsed, parsed_body) = Frontmatter::parse(&rendered).unwrap();
        assert_eq!(parsed.title, original.title);
        assert_eq!(parsed.base, "main");
        assert_eq!(parsed.labels, original.labels);
        assert!(!parsed.draft);
        assert_eq!(parsed_body.trim(), body.trim());
    }

    #[test]
    fn render_quotes_titles_containing_yaml_specials() {
        let original = fm("feat(thing): do the thing");
        let rendered = original.render("body", None).unwrap();
        let (parsed, _) = Frontmatter::parse(&rendered).unwrap();
        assert_eq!(parsed.title, "feat(thing): do the thing");
    }

    #[test]
    fn parse_supports_minimal_frontmatter() {
        let buffer = "---\ntitle: hello\nbase: main\n---\n\nbody text\n";
        let (fm, body) = Frontmatter::parse(buffer).unwrap();
        assert_eq!(fm.title, "hello");
        assert_eq!(fm.base, "main");
        assert!(fm.labels.is_empty());
        assert!(!fm.draft);
        assert!(!fm.auto_merge);
        assert_eq!(fm.auto_merge_method, AutoMergeMethod::Merge);
        assert_eq!(body, "body text");
    }

    #[test]
    fn render_omits_auto_merge_behavior_by_default() {
        let data = fm("feat: hi :)");
        let rendered = data.render("", None).unwrap();
        assert!(!rendered.contains("auto_merge_method:"));
    }

    #[test]
    fn parse_reads_auto_merge_fields() {
        let buffer = "---\ntitle: hello\nbase: main\nauto_merge: true\nauto_merge_method: squash\n---\n\nbody\n";
        let (fm, _) = Frontmatter::parse(buffer).unwrap();
        assert!(fm.auto_merge);
        assert_eq!(fm.auto_merge_method, AutoMergeMethod::Squash);
    }

    #[test]
    fn missing_leading_delimiter_errors() {
        let err = Frontmatter::parse("title: x\n---\nbody\n").unwrap_err();
        assert!(err.to_string().contains("missing leading"));
    }

    #[test]
    fn unterminated_frontmatter_errors() {
        let err = Frontmatter::parse("---\ntitle: x\nno end marker").unwrap_err();
        assert!(err.to_string().contains("unterminated"));
    }

    #[test]
    fn rendered_block_starts_with_delimiter_and_has_blank_line_before_body() {
        let rendered = fm("t").render("body", None).unwrap();
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.ends_with("\n---\n\nbody"));
        assert!(!rendered.contains("\n---\n\n\n"));
    }

    #[test]
    fn non_empty_body_has_exactly_one_blank_line_after_frontmatter() {
        let rendered = fm("t").render("body here\n", None).unwrap();
        assert!(rendered.ends_with("\n---\n\nbody here\n"));
    }

    #[test]
    fn parse_strips_preview_marker_and_diff() {
        let buffer = format!(
            "---\ntitle: hello\nbase: main\n---\n\nuser body\n\n{PREVIEW_MARKER}\n\n```diff\n- a\n+ b\n```\n"
        );
        let (fm, body) = Frontmatter::parse(&buffer).unwrap();
        assert_eq!(fm.title, "hello");
        assert_eq!(body, "user body");
    }

    #[test]
    fn parse_strips_with_blank_template_skeleton() {
        let buffer = format!(
            "---\ntitle: hello\nbase: main\n---\n\n\n\n{PREVIEW_MARKER}\n\n```diff\ndiff content\n```\n"
        );
        let (_, body) = Frontmatter::parse(&buffer).unwrap();
        assert_eq!(body, "");
    }

    #[test]
    fn parse_marker_absent_is_unchanged() {
        let buffer = "---\ntitle: hi\nbase: main\n---\n\nplain body without marker\n";
        let (_, body) = Frontmatter::parse(buffer).unwrap();
        assert_eq!(body, "plain body without marker");
    }

    #[test]
    fn parse_strips_at_first_marker_occurrence() {
        let buffer = format!(
            "---\ntitle: x\nbase: main\n---\n\nfirst body\n{PREVIEW_MARKER}\n\nmiddle\n{PREVIEW_MARKER}\n\nlast\n"
        );
        let (_, body) = Frontmatter::parse(&buffer).unwrap();
        assert_eq!(body, "first body");
    }

    #[test]
    fn parse_marker_with_trailing_whitespace_still_matches() {
        let with_trailing = format!("{PREVIEW_MARKER}   ");
        let buffer = format!("---\ntitle: x\nbase: main\n---\n\nbody\n{with_trailing}\n\nstuff\n");
        let (_, body) = Frontmatter::parse(&buffer).unwrap();
        assert_eq!(body, "body");
    }

    struct CaptureEditor(Mutex<Option<String>>);
    impl Editor for CaptureEditor {
        async fn edit(&self, _argv: &[String], initial: &str) -> Result<String> {
            *self.0.lock().unwrap() = Some(initial.to_string());
            Ok(initial.to_string())
        }
    }
    #[tokio::test]
    async fn round_trip_without_preview_does_not_inject_marker() {
        let fm = sample_fm();
        let editor = CaptureEditor(Mutex::new(None));
        editor::round_trip(&editor, &["x".into()], &fm, "body\n", None)
            .await
            .unwrap();
        let buf = editor.0.lock().unwrap().clone().unwrap();
        assert!(!buf.contains(PREVIEW_MARKER));
    }

    fn sample_fm() -> Frontmatter {
        Frontmatter {
            title: "t".into(),
            base: "main".into(),
            labels: vec![],
            reviewers: vec![],
            draft: false,
            auto_merge: false,
            auto_merge_method: AutoMergeMethod::Merge,
        }
    }

    struct EchoEditor;
    impl Editor for EchoEditor {
        async fn edit(&self, _argv: &[String], initial: &str) -> Result<String> {
            Ok(initial.to_string())
        }
    }

    #[tokio::test]
    async fn round_trip_with_preview_strips_diff_on_parse() {
        let fm = sample_fm();
        let editor = EchoEditor;
        let (parsed_fm, parsed_body) = editor::round_trip(
            &editor,
            &["x".into()],
            &fm,
            "real body content\n",
            Some("- a\n+ b\n"),
        )
        .await
        .unwrap();
        assert_eq!(parsed_fm.title, "t");
        assert_eq!(parsed_body, "real body content");
    }

    #[tokio::test]
    async fn round_trip_blank_body_with_preview_yields_empty_body() {
        let fm = sample_fm();
        let editor = EchoEditor;
        let (_, body) = editor::round_trip(&editor, &["x".into()], &fm, "", Some("diff"))
            .await
            .unwrap();
        assert_eq!(body, "");
    }

    fn fm_with_base(title: &str, base: &str) -> Frontmatter {
        Frontmatter {
            title: title.into(),
            base: base.into(),
            labels: vec![],
            reviewers: vec![],
            draft: false,
            auto_merge: false,
            auto_merge_method: AutoMergeMethod::Merge,
        }
    }

    #[test]
    fn validate_happy_path() {
        fm("title").validate().unwrap();
    }

    #[test]
    fn validate_empty_title_errors() {
        let err = fm("   ").validate().unwrap_err();
        assert!(err.to_string().contains("title is empty"));
    }

    #[test]
    fn validate_empty_base_errors() {
        let err = fm_with_base("title", "  ").validate().unwrap_err();
        assert!(err.to_string().contains("base is empty"));
    }

    #[test]
    fn validate_long_title_warns_but_passes() {
        let long_title = "x".repeat(TITLE_WARN_LEN + 1);
        fm(&long_title).validate().unwrap();
    }
}
