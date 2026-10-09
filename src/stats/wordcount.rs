//! The single definition of "word" for this project.
//!
//! Mirrors `wordcount.py` exactly. The two are kept in agreement by
//! `tests/wordcount_parity.rs`, which runs them both against the real
//! databases. Do not change one without the other.

use regex::Regex;
use std::sync::OnceLock;

/// `<style>` and `<script>` element *contents* are markup, not prose.
/// Two patterns rather than one with a backreference, because Rust's
/// `regex` crate has no backreferences. Verified to produce counts
/// identical to Python's `<(style|script)\b[^>]*>.*?</\1\s*>` on all
/// 2162 real chapters.
fn style_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").unwrap())
}

fn script_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap())
}

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}

/// Count words in a chapter's stored HTML.
///
/// Entities are deliberately not decoded: `&nbsp;` is part of a token,
/// matching the Python implementation's behaviour.
pub fn count_words(html: &str) -> usize {
    let without_style = style_re().replace_all(html, " ");
    let without_script = script_re().replace_all(&without_style, " ");
    let text = tag_re().replace_all(&without_script, " ");
    text.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::count_words;

    #[test]
    fn counts_plain_prose() {
        assert_eq!(count_words("<p>one two three</p>"), 3);
    }

    #[test]
    fn excludes_style_element_contents() {
        // Royal Road injects a honeypot style block into every chapter.
        // Its CSS text must not be counted as prose.
        let html = r#"<head><style>
            .cmY4NDFlNDA1{ display: none; speak: never; }
        </style></head><body><p>real words here</p></body>"#;
        assert_eq!(count_words(html), 3);
    }

    #[test]
    fn excludes_script_element_contents() {
        let html = r#"<script>document.createElement('audio');</script><p>one two</p>"#;
        assert_eq!(count_words(html), 2);
    }

    #[test]
    fn does_not_decode_entities() {
        // The Python implementation never decoded entities, so neither do we.
        // &nbsp; is a token, not a space.
        assert_eq!(count_words("<p>a&nbsp;b</p>"), 1);
    }

    #[test]
    fn counts_heading_text() {
        // Chapter titles are inside the counted region and always have been.
        assert_eq!(count_words("<h1>Chapter One</h1><p>body text</p>"), 4);
    }

    #[test]
    fn handles_unicode_and_collapses_whitespace() {
        assert_eq!(count_words("<p>café   \n\t  ünïcode</p>"), 2);
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("<p></p>"), 0);
    }
}
