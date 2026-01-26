use color_name::Color;
use regex::Regex;

use crate::postprocess::PostProcessor;

/// Post-processor that converts colored spans to text representation
pub struct StripColourProcessor;

impl PostProcessor for StripColourProcessor {
    fn name(&self) -> &str {
        "strip-colour"
    }

    fn process(&self, content: &str) -> String {
        let re = Regex::new(r#"<span style="color:\s*(#......).*?">(.*?)</span>"#).unwrap();
        re.replace_all(content, |captures: &regex::Captures| {
            let colour_arr = hex::decode(&captures[1][1..]).unwrap();
            let name = Color::similar([colour_arr[0], colour_arr[1], colour_arr[2]]);
            format!(
                "<span>&lt;{a}|{b}|{a}&gt;</span>",
                a = name,
                b = &captures[2]
            )
        })
        .to_string()
    }
}
