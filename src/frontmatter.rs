//! YAML frontmatter for the PR editor buffer.
//!
//! Layout:
//! ```````text
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
//! `````diff
//! - line removed
//! + line added
//! `````
//! ```````

use crate::{
    config::{AutoMergeMethod, AutoMergeMode},
    gh::Reviewer,
};
use anyhow::{Context, Result, anyhow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::ops::Not;

/// Sentinel heading that marks the start of the read-only diff preview in the
/// `pr create` editor buffer. The marker line and everything after it are
/// stripped on parse. Whole-line exact match (after trimming trailing
/// whitespace).
pub const PREVIEW_MARKER: &str = "8< jj-gh: below this line removed on submit >8";

const DIFF_FENCE_START: &str = "`````diff";
const DIFF_FENCE_END: &str = "`````";

const TITLE_WARN_LEN: usize = 72;

/// Editor schema for this version. Release assets keep old schemas accessible
/// even after the docs site is redeployed for a newer release.
const SCHEMA_URL: &str = concat!(
    "https://github.com/mrjones2014/jj-gh/releases/download/jj-gh-v",
    env!("CARGO_PKG_VERSION"),
    "/frontmatter.schema.json"
);

fn is_default<T: Default + PartialEq>(val: &T) -> bool {
    *val == T::default()
}

/// The YAML frontmatter at the top of a `pr create` / `pr edit` buffer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "jj-gh PR frontmatter")]
pub struct Frontmatter {
    /// PR title; required and non-empty.
    pub title: String,
    /// Base branch; use `owner:branch` for cross-fork PRs.
    pub base: String,
    /// Label names to apply to the PR.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Reviewers as `@user` or `@org/team` slugs.
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub reviewers: Vec<Reviewer>,
    /// Whether the PR is a draft.
    #[serde(default)]
    pub draft: bool,
    /// Auto-merge scope. Omit this field when the current request cannot be
    /// represented by one of the four values; submitting it unchanged then
    /// leaves the request alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<AutoMergeMode>,
    /// Merge method when auto-merge is enabled.
    #[serde(default, skip_serializing_if = "is_default")]
    pub auto_merge_method: AutoMergeMethod,
    /// Rendered as a YAML comment above `auto_merge` (or where it would be).
    /// Not parsed back; it only explains a state the enum cannot express.
    #[serde(skip)]
    pub auto_merge_comment: Option<String>,
}

impl Frontmatter {
    /// Add the auto-merge explanation directly into the YAML block so it is
    /// visible in the editor. Comments are stripped by the YAML parser, so the
    /// round-trip is unaffected.
    fn annotated_yaml(&self, yaml: &str) -> String {
        let mut notes = vec![
            "# auto_merge options: false, infer, all, below".to_string(),
            "#   false = cancel; infer = whole stack if stacked, otherwise this PR".to_string(),
            "#   all = whole GitHub stack through its top PR; below = this PR and its descendants"
                .to_string(),
        ];
        if let Some(note) = &self.auto_merge_comment {
            notes.push(format!("# {note}"));
        }
        let notes = notes.join("\n");

        let mut out = String::new();
        let mut inserted = false;
        for line in yaml.lines() {
            if !inserted
                && (line.starts_with("auto_merge:") || line.starts_with("auto_merge_method:"))
            {
                out.push_str(&notes);
                out.push('\n');
                inserted = true;
            }
            out.push_str(line);
            out.push('\n');
        }
        if !inserted {
            out.push_str(&notes);
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    /// Emit YAML block + blank line + body for the editor buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML serializer fails (unlikely for our fields).
    pub fn render(&self, body: &str, preview: Option<&str>) -> Result<String> {
        let yaml = noyalib::to_string(self).context("could not serialize frontmatter")?;
        let yaml = self.annotated_yaml(&yaml);
        let orig_body_empty = body.trim().is_empty();
        let body = body.trim_start_matches('\n');
        let body = if body.is_empty() { "\n" } else { body };
        let body =
            format!("---\n# yaml-language-server: $schema={SCHEMA_URL}\n{yaml}\n---\n\n{body}");
        let Some(diff) = preview else {
            return Ok(body);
        };

        // apply some extra `\n` for comfort in the editor
        if orig_body_empty {
            Ok(format!(
                "{body}\n\n\n\n# {PREVIEW_MARKER}\n\n{DIFF_FENCE_START}\n{diff}\n{DIFF_FENCE_END}\n"
            ))
        } else {
            let trimmed = body.trim_end();
            Ok(format!(
                "{trimmed}\n\n\n\n\n# {PREVIEW_MARKER}\n\n{DIFF_FENCE_START}\n{diff}\n{DIFF_FENCE_END}\n"
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
        // normalize `\r\n` to `\n`
        let buffer = buffer.replace("\r\n", "\n");
        let trimmed = strip_preview(&buffer);
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
/// Matches by trimming leading markdown heading syntax entirely, then checking
/// for a match, this makes it robust against misbehaving markdown formatters.
fn strip_preview(buffer: &str) -> &str {
    let user_body_len = buffer
        .split_inclusive('\n')
        .take_while(|line| line_is_marker(line).not())
        .map(str::len)
        .sum();
    &buffer[..user_body_len]
}

fn line_is_marker(line: &&str) -> bool {
    line.trim_start_matches(|c: char| c == '#' || c.is_whitespace())
        .trim()
        .eq(PREVIEW_MARKER)
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
            auto_merge: Some(AutoMergeMode::False),
            auto_merge_method: AutoMergeMethod::Merge,
            auto_merge_comment: None,
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
    fn render_includes_schema_directive_for_editors() {
        let rendered = fm("title").render("body", None).unwrap();
        assert!(
            rendered.starts_with(&format!(
                "---\n# yaml-language-server: $schema={SCHEMA_URL}\n"
            )),
            "{rendered}"
        );
        assert!(SCHEMA_URL.contains(env!("CARGO_PKG_VERSION")));
        // The directive must not disturb the frontmatter round trip.
        Frontmatter::parse(&rendered).unwrap();
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
        assert_eq!(fm.labels, [] as [String; 0]);
        assert!(!fm.draft);
        assert_eq!(fm.auto_merge, None);
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
    fn parse_reads_auto_merge_values() {
        for (value, expected) in [
            ("false", AutoMergeMode::False),
            ("infer", AutoMergeMode::Infer),
            ("all", AutoMergeMode::All),
            ("below", AutoMergeMode::Below),
        ] {
            let buffer = format!(
                "---\ntitle: hello\nbase: main\nauto_merge: {value}\nauto_merge_method: squash\n---\n\nbody\n"
            );
            let (fm, _) = Frontmatter::parse(&buffer).unwrap();
            assert_eq!(fm.auto_merge, Some(expected), "value: {value}");
            assert_eq!(fm.auto_merge_method, AutoMergeMethod::Squash);
        }
    }

    #[test]
    fn parse_accepts_legacy_true_as_infer() {
        let buffer = "---\ntitle: hello\nbase: main\nauto_merge: true\n---\n\nbody\n";
        let (fm, _) = Frontmatter::parse(buffer).unwrap();
        assert_eq!(fm.auto_merge, Some(AutoMergeMode::Infer));
    }

    #[test]
    fn parse_rejects_unknown_auto_merge_value() {
        let buffer = "---\ntitle: hello\nbase: main\nauto_merge: through-2\n---\n\nbody\n";
        Frontmatter::parse(buffer).expect_err("`through-2` is not a valid auto_merge value");
    }

    #[test]
    fn render_emits_explicit_value_with_explanation() {
        let mut data = fm("t");
        data.auto_merge = Some(AutoMergeMode::All);
        let rendered = data.render("body", None).unwrap();
        assert!(rendered.contains("auto_merge: all"), "{rendered}");
        assert!(rendered.contains("# auto_merge options:"), "{rendered}");
        let (parsed, _) = Frontmatter::parse(&rendered).unwrap();
        assert_eq!(parsed.auto_merge, Some(AutoMergeMode::All));
    }

    #[test]
    fn render_omits_auto_merge_and_keeps_endpoint_note_in_comment() {
        let mut data = fm("t");
        data.auto_merge = None;
        data.auto_merge_comment = Some("auto-merge is requested through PR #3".into());
        let rendered = data.render("body", None).unwrap();
        assert!(!rendered.contains("auto_merge:"), "{rendered}");
        assert!(
            rendered.contains("# auto-merge is requested through PR #3"),
            "{rendered}"
        );
        let (parsed, _) = Frontmatter::parse(&rendered).unwrap();
        assert_eq!(parsed.auto_merge, None);
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
            "---\ntitle: hello\nbase: main\n---\n\nuser body\n\n{PREVIEW_MARKER}\n\n{DIFF_FENCE_START}\n- a\n+ b\n{DIFF_FENCE_END}\n"
        );
        let (fm, body) = Frontmatter::parse(&buffer).unwrap();
        assert_eq!(fm.title, "hello");
        assert_eq!(body, "user body");
    }

    #[test]
    fn parse_strips_with_blank_template_skeleton() {
        let buffer = format!(
            "---\ntitle: hello\nbase: main\n---\n\n\n\n{PREVIEW_MARKER}\n\n{DIFF_FENCE_START}\ndiff content\n{DIFF_FENCE_END}\n"
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
        fn edit(&self, _argv: &[String], initial: &str) -> impl Future<Output = Result<String>> {
            *self.0.lock().unwrap() = Some(initial.to_string());
            std::future::ready(Ok(initial.to_string()))
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
            auto_merge: Some(AutoMergeMode::False),
            auto_merge_method: AutoMergeMethod::Merge,
            auto_merge_comment: None,
        }
    }

    struct EchoEditor;
    impl Editor for EchoEditor {
        fn edit(&self, _argv: &[String], initial: &str) -> impl Future<Output = Result<String>> {
            std::future::ready(Ok(initial.to_string()))
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
            auto_merge: Some(AutoMergeMode::False),
            auto_merge_method: AutoMergeMethod::Merge,
            auto_merge_comment: None,
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

    #[test]
    fn json_schema_describes_the_editor_frontmatter() {
        let schema = serde_json::to_value(schemars::schema_for!(Frontmatter)).unwrap();
        let properties = schema["properties"].as_object().unwrap();

        for field in [
            "title",
            "base",
            "labels",
            "reviewers",
            "draft",
            "auto_merge",
        ] {
            assert!(
                properties.contains_key(field),
                "missing schema field: {field}"
            );
        }
        assert!(!properties.contains_key("auto_merge_comment"));
        assert_eq!(schema["required"], serde_json::json!(["title", "base"]));

        let mode = &schema["$defs"]["AutoMergeMode"];
        assert_eq!(
            mode["anyOf"][0]["enum"],
            serde_json::json!(["false", "infer", "all", "below"]),
            "auto_merge values should match the editor keywords"
        );
        assert_eq!(
            mode["anyOf"][1]["type"],
            serde_json::json!("boolean"),
            "auto_merge should also accept YAML booleans"
        );
    }
}
