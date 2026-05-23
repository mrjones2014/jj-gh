//! Bookmark-template rendering and validation.
//!
//! Single-pass parser over a `{field}` placeholder syntax. `{{` / `}}` are
//! literal braces; everything else is text.

use anyhow::{Result, anyhow};

pub const DEFAULT_FETCH_TEMPLATE: &str = "pr-{number}/{branch}";

/// All placeholders recognized in a bookmark template.
pub const VALID_PLACEHOLDERS: &[&str] = &["number", "branch", "user", "repo"];

/// Field values supplied to [`render`]. `user` / `repo` are `None` when the
/// GitHub API returns null (e.g. the source fork was deleted).
pub struct Fields<'a> {
    pub number: u64,
    pub branch: &'a str,
    pub user: Option<&'a str>,
    pub repo: Option<&'a str>,
}

/// Render `template` substituting `{field}` placeholders with `fields`.
///
/// # Errors
///
/// Returns an error if the template contains an unknown placeholder, has
/// an unterminated `{`, has an empty `{}`, or references a placeholder for
/// which the corresponding [`Fields`] value is `None`.
pub fn render(template: &str, fields: &Fields) -> Result<String> {
    parse(template, |name| substitute(name, fields))
}

/// Validate that all `{...}` placeholders in `template` are known. Does not
/// require any field values.
///
/// # Errors
///
/// See [`render`] (minus the unset-field case).
pub fn validate(template: &str) -> Result<()> {
    parse(template, |name| {
        if VALID_PLACEHOLDERS.contains(&name) {
            Ok(String::new())
        } else {
            Err(unknown_placeholder_err(name))
        }
    })?;
    Ok(())
}

fn substitute(name: &str, fields: &Fields) -> Result<String> {
    match name {
        "number" => Ok(fields.number.to_string()),
        "branch" => Ok(fields.branch.to_string()),
        "user" => fields.user.map(str::to_string).ok_or_else(|| {
            anyhow!("`{{user}}` placeholder unavailable: head.user is null (fork deleted?)")
        }),
        "repo" => fields.repo.map(str::to_string).ok_or_else(|| {
            anyhow!("`{{repo}}` placeholder unavailable: head.repo is null (fork deleted?)")
        }),
        other => Err(unknown_placeholder_err(other)),
    }
}

fn unknown_placeholder_err(name: &str) -> anyhow::Error {
    anyhow!(
        "unknown placeholder `{{{name}}}` in bookmark template; valid: {}",
        VALID_PLACEHOLDERS
            .iter()
            .map(|p| format!("{{{p}}}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse<F>(template: &str, mut on_placeholder: F) -> Result<String>
where
    F: FnMut(&str) -> Result<String>,
{
    let mut out = String::with_capacity(template.len());
    let mut chars = template.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            '{' => {
                if matches!(chars.peek(), Some((_, '{'))) {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut name = String::new();
                let mut closed = false;
                for (_, nc) in chars.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if !closed {
                    return Err(anyhow!("unterminated `{{` in template"));
                }
                if name.is_empty() {
                    return Err(anyhow!("empty placeholder `{{}}` in template"));
                }
                out.push_str(&on_placeholder(&name)?);
            }
            '}' => {
                if matches!(chars.peek(), Some((_, '}'))) {
                    chars.next();
                    out.push('}');
                } else {
                    return Err(anyhow!("stray `}}` in template; use `}}}}` for a literal"));
                }
            }
            _ => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Fields<'static> {
        Fields {
            number: 1234,
            branch: "feature/foo",
            user: Some("octocat"),
            repo: Some("r"),
        }
    }

    #[test]
    fn default_template_round_trip() {
        assert_eq!(
            render(DEFAULT_FETCH_TEMPLATE, &fields()).unwrap(),
            "pr-1234/feature/foo"
        );
    }

    #[test]
    fn each_placeholder_substitutes() {
        assert_eq!(render("{number}", &fields()).unwrap(), "1234");
        assert_eq!(render("{branch}", &fields()).unwrap(), "feature/foo");
        assert_eq!(render("{user}", &fields()).unwrap(), "octocat");
        assert_eq!(render("{repo}", &fields()).unwrap(), "r");
    }

    #[test]
    fn unknown_placeholder_lists_valid_set() {
        let err = render("pr-{nope}", &fields()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("{nope}"), "msg: {msg}");
        assert!(msg.contains("{number}"), "msg: {msg}");
        assert!(msg.contains("{branch}"), "msg: {msg}");
        assert!(msg.contains("{user}"), "msg: {msg}");
        assert!(msg.contains("{repo}"), "msg: {msg}");
    }

    #[test]
    fn double_brace_escapes_literal() {
        assert_eq!(
            render("{{not-a-placeholder}}", &fields()).unwrap(),
            "{not-a-placeholder}"
        );
    }

    #[test]
    fn unterminated_brace_errors() {
        let err = render("pr-{number", &fields()).unwrap_err();
        assert!(err.to_string().contains("unterminated"), "msg: {err}");
    }

    #[test]
    fn empty_placeholder_errors() {
        let err = render("pr-{}", &fields()).unwrap_err();
        assert!(err.to_string().contains("empty"), "msg: {err}");
    }

    #[test]
    fn stray_close_brace_errors() {
        let err = render("pr-}foo", &fields()).unwrap_err();
        assert!(err.to_string().contains("stray"), "msg: {err}");
    }

    #[test]
    fn validate_accepts_default() {
        validate(DEFAULT_FETCH_TEMPLATE).unwrap();
    }

    #[test]
    fn validate_rejects_unknown() {
        let err = validate("pr-{nope}").unwrap_err();
        assert!(err.to_string().contains("{nope}"), "msg: {err}");
    }

    #[test]
    fn validate_does_not_require_values() {
        // user/repo references should still validate even though no Fields
        validate("pr-{user}-{repo}").unwrap();
    }

    #[test]
    fn null_user_errors_at_render() {
        let f = Fields {
            number: 1,
            branch: "b",
            user: None,
            repo: Some("r"),
        };
        let err = render("{user}", &f).unwrap_err();
        assert!(err.to_string().contains("{user}"), "msg: {err}");
    }
}
