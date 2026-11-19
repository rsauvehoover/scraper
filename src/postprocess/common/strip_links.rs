use regex::Regex;

use crate::postprocess::PostProcessor;
use crate::sources::content_images::unwrap_image_links;

/// Post-processor that removes all HTML links from content
pub struct StripLinksProcessor;

impl PostProcessor for StripLinksProcessor {
    fn name(&self) -> &str {
        "strip-links"
    }

    fn process(&self, content: &str) -> String {
        let re = Regex::new(r"<a.*?</a>").unwrap();
        re.replace_all(&unwrap_image_links(content), "").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_links_keeps_a_linked_image() {
        let out = StripLinksProcessor.process(r#"<a href="/n">Next</a><a href="/i"><img src="https://example.com/a.png"/></a>"#);
        assert_eq!(out, r#"<img src="https://example.com/a.png"/>"#);
    }
}
