//! Server-side sanitisation of stored chapter HTML.
//!
//! The scraper stores whatever upstream serves, verbatim. 40 chapters already
//! contain `<script>` or `<iframe>` — benign WordPress audio shortcodes today,
//! but a compromised upstream would be stored identically. This is the first of
//! two independent defences; the second is the sandboxed iframe and CSP in
//! `toc.rs`. Either alone is one bug away from running upstream script in the
//! origin that edits the mail app password.

use std::collections::HashSet;
use std::sync::OnceLock;

use ammonia::Builder;

fn cleaner() -> &'static Builder<'static> {
    static CLEANER: OnceLock<Builder<'static>> = OnceLock::new();
    CLEANER.get_or_init(|| {
        let mut builder = Builder::default();

        let tags: HashSet<&str> = [
            "p", "br", "hr", "div", "span", "blockquote", "pre", "code",
            "em", "i", "strong", "b", "u", "s", "sub", "sup", "small",
            "h1", "h2", "h3", "h4", "h5", "h6",
            "ul", "ol", "li", "dl", "dt", "dd",
            "table", "thead", "tbody", "tr", "th", "td",
            "img", "figure", "figcaption",
        ]
        .iter()
        .copied()
        .collect();
        builder.tags(tags);

        // Inline `style` is allowed because colour carries meaning in these
        // serials, and the strip-colour processor is an explicit opt-in
        // elsewhere. Ammonia does NOT filter style values unless asked, so the
        // allowlist below is what makes that safe: it admits only properties
        // that cannot carry a `url()`, `expression()` or `behavior`, which are
        // the vectors that would otherwise fetch or execute from inside a
        // chapter. Without this call, `style` would be passed through verbatim
        // and the CSP would be the only thing standing between a stored
        // payload and the reader.
        let mut attrs = HashSet::new();
        attrs.insert("style");
        builder.generic_attributes(attrs);

        builder.filter_style_properties(
            [
                "color",
                "background-color",
                "font-weight",
                "font-style",
                "font-variant",
                "text-decoration",
                "text-align",
                "letter-spacing",
            ]
            .iter()
            .copied()
            .collect(),
        );

        let mut img_attrs = HashSet::new();
        img_attrs.insert("src");
        img_attrs.insert("alt");
        builder.add_tag_attributes("img", img_attrs);

        // Links are stripped to text: chapter HTML is full of navigation
        // links that mean nothing in this context, and an href is one more
        // thing to get wrong.
        builder.link_rel(None);

        builder
    })
}

/// Sanitise one chapter's stored HTML for display.
pub fn sanitize_chapter(html: &str) -> String {
    cleaner().clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_chapter;

    #[test]
    fn strips_script_elements() {
        let out = sanitize_chapter("<p>before</p><script>alert(1)</script><p>after</p>");
        assert!(!out.contains("<script"));
        assert!(!out.contains("alert(1)"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    #[test]
    fn strips_iframes() {
        let out = sanitize_chapter(r#"<iframe src="https://evil.example/"></iframe><p>text</p>"#);
        assert!(!out.contains("<iframe"));
        assert!(out.contains("text"));
    }

    #[test]
    fn strips_inline_event_handlers() {
        let out = sanitize_chapter(r#"<p onclick="steal()">text</p>"#);
        assert!(!out.contains("onclick"));
        assert!(out.contains("text"));
    }

    #[test]
    fn strips_javascript_urls() {
        let out = sanitize_chapter(r#"<a href="javascript:alert(1)">link</a>"#);
        assert!(!out.contains("javascript:"));
    }

    #[test]
    fn strips_forms() {
        let out = sanitize_chapter(r#"<form action="/config"><input name="x"></form><p>text</p>"#);
        assert!(!out.contains("<form"));
        assert!(!out.contains("<input"));
    }

    #[test]
    fn preserves_prose_structure_and_emphasis() {
        let out = sanitize_chapter("<p>plain <em>emphasis</em> and <strong>strong</strong></p>");
        assert!(out.contains("<em>"));
        assert!(out.contains("<strong>"));
        assert!(out.contains("<p>"));
    }

    #[test]
    fn preserves_inline_colour_styling() {
        // Colour is meaningful in these serials; the strip-colour processor is
        // an explicit opt-in, so the reader must not silently remove it.
        let out = sanitize_chapter(r#"<span style="color: #cc4125;">red text</span>"#);
        assert!(out.contains("color"), "inline colour must survive: {}", out);
        assert!(out.contains("red text"));
    }

    #[test]
    fn is_idempotent() {
        let once = sanitize_chapter("<p>text</p><script>x</script>");
        assert_eq!(sanitize_chapter(&once), once);
    }

    #[test]
    fn strips_style_properties_that_can_fetch_or_execute() {
        let out = sanitize_chapter(
            r#"<span style="background: url(https://evil.example/p.png); color: red; behavior:url(x.htc)">text</span>"#,
        );
        assert!(!out.contains("url("), "style url() survived: {}", out);
        assert!(!out.contains("behavior"), "behavior survived: {}", out);
        assert!(out.contains("color"), "colour must still survive: {}", out);
        assert!(out.contains("text"));
    }
}
