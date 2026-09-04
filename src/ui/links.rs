//! PR number to URL lookup, so terminal output can render `#123` as a
//! clickable hyperlink.
//!
//! URLs come from the API rather than being synthesized from owner/repo:
//! GitHub Enterprise hosts differ, and a wrong link is worse than none.

use crate::{
    gh::PrDetails,
    ui::{Hyperlink, Stream},
};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct PrLinks(HashMap<u64, String>);

impl FromIterator<(u64, String)> for PrLinks {
    fn from_iter<I: IntoIterator<Item = (u64, String)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl PrLinks {
    #[must_use]
    pub fn from_details(details: &[PrDetails]) -> Self {
        details
            .iter()
            .map(|pr| (pr.number, pr.html_url.clone()))
            .collect()
    }

    /// `text` as a hyperlink to `number`'s PR. Left alone when the URL is
    /// unknown or `stream` cannot render one.
    ///
    /// Deliberately not underlined: this is for spans wider than their label,
    /// such as a whole plan row, where [`PrLinks::underlined_number`] marks the
    /// clickable part instead.
    #[must_use]
    pub fn link(&self, stream: Stream, number: u64, text: &str) -> String {
        self.0.get(&number).map_or_else(
            || text.to_string(),
            |url| text.hyperlink(stream, url.trim()),
        )
    }

    /// `#123`, underlined and hyperlinked when possible.
    #[must_use]
    pub fn number(&self, stream: Stream, number: u64) -> String {
        let label = format!("#{number}");
        self.0.get(&number).map_or_else(
            || label.clone(),
            |url| stream.underline(&label).hyperlink(stream, url.trim()),
        )
    }

    /// `#123` underlined but not itself linked, for use inside a span that is
    /// already a hyperlink: a nested OSC 8 sequence would close the outer link
    /// early and leave the rest of the row dead.
    #[must_use]
    pub fn underlined_number(&self, stream: Stream, number: u64) -> String {
        let label = format!("#{number}");
        if self.0.contains_key(&number) {
            stream.underline(&label)
        } else {
            label
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_numbers_render_plain() {
        let links = [(12, "https://gh/o/r/pull/12".to_string())]
            .into_iter()
            .collect::<PrLinks>();
        assert_eq!(links.number(Stream::Stdout, 99), "#99");
    }
}
