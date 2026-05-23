//! YAML frontmatter for the PR editor buffer.
//!
//! Layout:
//! ```text
//! ---
//! title: ...
//! base: main
//! labels: [...]
//! draft: false
//! ---
//!
//! <body>
//! ```

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    pub title: String,
    pub base: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub draft: bool,
}

impl Frontmatter {
    /// Emit YAML block + blank line + body for the editor buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML serializer fails (unlikely for our fields).
    pub fn render(&self, body: &str) -> Result<String> {
        let yaml = serde_yml::to_string(self).context("could not serialize frontmatter")?;
        let body = body.trim_start_matches('\n');
        Ok(format!("---\n{yaml}---\n\n\n{body}"))
    }

    /// Parse a frontmatter-prefixed markdown buffer back into `(meta, body)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the document is missing or has an unterminated
    /// frontmatter block, or if the YAML fails to deserialize.
    pub fn parse(buffer: &str) -> Result<(Self, String)> {
        let rest = buffer
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow!("missing leading `---` frontmatter delimiter"))?;
        let (yaml, body) = rest
            .split_once("\n---\n")
            .or_else(|| rest.split_once("\n---"))
            .ok_or_else(|| anyhow!("unterminated frontmatter; expected closing `---`"))?;
        let fm: Frontmatter =
            serde_yml::from_str(yaml).context("could not parse YAML frontmatter")?;
        let body = body.trim_start_matches('\n').to_string();
        Ok((fm, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(title: &str) -> Frontmatter {
        Frontmatter {
            title: title.into(),
            base: "main".into(),
            labels: vec!["bug".into(), "p1".into()],
            draft: false,
        }
    }

    #[test]
    fn render_round_trips_through_parse() {
        let original = fm("Fix the thing");
        let body = "This PR does X and Y.\n";
        let rendered = original.render(body).unwrap();

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
        let rendered = original.render("body").unwrap();
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
        assert_eq!(body, "body text\n");
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
        let rendered = fm("t").render("body").unwrap();
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.contains("\n---\n\n"));
    }
}
