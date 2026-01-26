use regex::Regex;

use super::PostProcessor;

/// Post-processor that converts Mrsha's writing style to italic emphasis
pub struct MrshaWriteProcessor;

impl PostProcessor for MrshaWriteProcessor {
    fn name(&self) -> &str {
        "mrsha-write"
    }

    fn process(&self, content: &str) -> String {
        let re = Regex::new(r#"<span.*?mrsha-write.*?>(.*?)</span>"#).unwrap();
        re.replace_all(content, |captures: &regex::Captures| {
            format!("<em>{}</em>", &captures[1])
        })
        .to_string()
    }
}
