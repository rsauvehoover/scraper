use regex::Regex;

use crate::postprocess::PostProcessor;

/// Post-processor that removes all HTML links from content
pub struct StripLinksProcessor;

impl PostProcessor for StripLinksProcessor {
    fn name(&self) -> &str {
        "strip-links"
    }

    fn process(&self, content: &str) -> String {
        let re = Regex::new(r"<a.*?</a>").unwrap();
        re.replace_all(content, "").to_string()
    }
}
