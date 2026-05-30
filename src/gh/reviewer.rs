//! A PR reviewer, identified by a GitHub user login or `org/team` team slug.
//!
//! Stored canonically as the slug ("john-doe" or "octo-org/security"), since
//! the API takes slugs and we'd otherwise reformat on every call. [`Display`]
//! and [`Serialize`] emit the user-facing `@`-prefixed form so frontmatter
//! YAML round-trips look like `reviewers: [@john-doe, @octo-org/security]`.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{fmt, str::FromStr};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reviewer(String);

impl Reviewer {
    /// Parse a reviewer from any of `@user`, `user`, `@org/team`, `org/team`.
    ///
    /// # Errors
    ///
    /// Empty/whitespace-only input, or a team slug with anything other than
    /// exactly one `/` (e.g. `org/team/extra`).
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim().trim_start_matches('@');
        if trimmed.is_empty() {
            return Err(anyhow!("reviewer is empty"));
        }
        let slash_count = trimmed.chars().filter(|c| *c == '/').count();
        if slash_count > 1 {
            return Err(anyhow!(
                "invalid team reviewer `{raw}`: team slug must be `org/team-name` (saw {slash_count} `/`)"
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// API-form slug: `john-doe` or `org/team-name`.
    #[must_use]
    pub fn slug(&self) -> &str {
        &self.0
    }

    /// User-facing form: `@john-doe` or `@org/team-name`.
    #[must_use]
    pub fn at_reference(&self) -> String {
        format!("@{}", self.0)
    }

    /// For a team slug `org/team-name`, the `team-name` portion. `None` for
    /// user reviewers. GitHub's REST review-request endpoint takes team names,
    /// not full `org/team` slugs.
    #[must_use]
    pub fn team_name(&self) -> Option<&str> {
        self.0.split_once('/').map(|(_, name)| name)
    }
}

impl fmt::Display for Reviewer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}

impl FromStr for Reviewer {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl Serialize for Reviewer {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.at_reference())
    }
}

impl<'de> Deserialize<'de> for Reviewer {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(de)?;
        Self::parse(&raw).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_with_at() {
        let r = Reviewer::parse("@john-doe").unwrap();
        assert_eq!(r.slug(), "john-doe");
        assert_eq!(r.at_reference(), "@john-doe");
        assert_eq!(r.team_name(), None);
    }

    #[test]
    fn parses_user_without_at() {
        let r = Reviewer::parse("john-doe").unwrap();
        assert_eq!(r.slug(), "john-doe");
    }

    #[test]
    fn parses_team_with_at() {
        let r = Reviewer::parse("@octo-org/security").unwrap();
        assert_eq!(r.slug(), "octo-org/security");
        assert_eq!(r.team_name(), Some("security"));
    }

    #[test]
    fn parses_team_without_at() {
        let r = Reviewer::parse("octo-org/security").unwrap();
        assert_eq!(r.slug(), "octo-org/security");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let r = Reviewer::parse("  @john  ").unwrap();
        assert_eq!(r.slug(), "john");
    }

    #[test]
    fn rejects_empty() {
        assert!(Reviewer::parse("").is_err());
        assert!(Reviewer::parse("   ").is_err());
        assert!(Reviewer::parse("@").is_err());
    }

    #[test]
    fn rejects_team_with_extra_slashes() {
        let err = Reviewer::parse("org/team/extra").unwrap_err();
        assert!(err.to_string().contains("team slug"));
    }

    #[test]
    fn display_uses_at_form() {
        let r = Reviewer::parse("john").unwrap();
        assert_eq!(format!("{r}"), "@john");
    }

    #[test]
    fn serde_round_trips_via_at_form() {
        let r = Reviewer::parse("john").unwrap();
        let yaml = serde_yml::to_string(&r).unwrap();
        // serde_yml wraps anything starting with `@` (a YAML reserved indicator).
        assert!(yaml.contains("@john"), "yaml was {yaml:?}");
        let back: Reviewer = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn deserialize_accepts_bare_slug() {
        let r: Reviewer = serde_yml::from_str("john").unwrap();
        assert_eq!(r.slug(), "john");
    }
}
