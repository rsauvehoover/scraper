//! Image tag handling for parsed chapter HTML.
//!
//! Parsers unwrap linked images and normalise every `<img>` to a fixed form so
//! later stages can fetch the URLs and rewrite `src` by exact string match.

use regex::Regex;

const IMG_STYLE: &str = "max-width:100%;height:auto";

fn anchor_re() -> Regex {
    Regex::new(r"(?s)<a\b[^>]*>(.*?)</a>").unwrap()
}

fn img_re() -> Regex {
    Regex::new(r"(?s)<img\b[^>]*>").unwrap()
}

fn attr_re() -> Regex {
    Regex::new(r#"(?s)\s(src|alt)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
}

/// Escape a value for a double-quoted attribute: `&` and `"`.
pub fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

fn entity_decode(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Replace every anchor that contains an image with just its image tags.
/// Anchors without an image are left for the strip-links step.
pub fn unwrap_image_links(html: &str) -> String {
    let imgs = img_re();
    anchor_re()
        .replace_all(html, |caps: &regex::Captures| {
            let tags: Vec<&str> = imgs.find_iter(&caps[1]).map(|m| m.as_str()).collect();
            if tags.is_empty() {
                caps[0].to_string()
            } else {
                tags.concat()
            }
        })
        .into_owned()
}

/// Fetch URL for a raw `src` value, or None if it cannot be fetched.
fn fetch_url(raw_src: &str) -> Option<String> {
    let decoded = entity_decode(raw_src.trim());
    let absolute = if let Some(rest) = decoded.strip_prefix("//") {
        format!("https://{}", rest)
    } else {
        decoded
    };
    if !(absolute.starts_with("http://") || absolute.starts_with("https://")) {
        return None;
    }
    let url = absolute.split('?').next().unwrap_or(&absolute);
    Some(url.to_string())
}

/// Normalise every `<img>` tag and return the fetch URLs in document order,
/// each once. Tags without a fetchable `src` are removed.
pub fn extract_images(html: &str) -> (String, Vec<String>) {
    let attrs = attr_re();
    let mut urls: Vec<String> = Vec::new();
    let out = img_re().replace_all(html, |caps: &regex::Captures| {
        let tag = &caps[0];
        let mut src: Option<String> = None;
        let mut alt: Option<String> = None;
        for a in attrs.captures_iter(tag) {
            let double = a.get(2).is_some();
            let value = a.get(2).or_else(|| a.get(3)).map_or("", |m| m.as_str());
            match &a[1] {
                "src" if src.is_none() => src = Some(value.to_string()),
                "alt" if alt.is_none() => {
                    alt = Some(if double {
                        value.to_string()
                    } else {
                        value.replace('"', "&quot;")
                    })
                }
                _ => {}
            }
        }
        let Some(url) = src.as_deref().and_then(fetch_url) else {
            return String::new();
        };
        let tag = format!(
            r#"<img src="{}" alt="{}" style="{}"/>"#,
            attr_escape(&url),
            alt.unwrap_or_default(),
            IMG_STYLE
        );
        if !urls.contains(&url) {
            urls.push(url);
        }
        tag
    });
    (out.into_owned(), urls)
}

/// Point every normalised tag for each `(url, path)` at its local path.
pub fn assign_image_paths(html: &str, assigned: &[(String, String)]) -> String {
    let mut out = html.to_string();
    for (url, path) in assigned {
        out = out.replace(
            &format!(r#"<img src="{}" alt="#, attr_escape(url)),
            &format!(r#"<img src="{}" alt="#, attr_escape(path)),
        );
    }
    out
}

/// Remove every normalised tag for `url`.
pub fn drop_image(html: &str, url: &str) -> String {
    let re = Regex::new(&format!(
        r#"<img src="{}" alt="[^"]*" style="{}"/>"#,
        regex::escape(&attr_escape(url)),
        regex::escape(IMG_STYLE)
    ))
    .unwrap();
    re.replace_all(html, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_wrapping_only_an_image_becomes_the_image() {
        let html = r#"<p><a href="https://example.com/big.png"><img src="https://example.com/x.png" alt="art"/></a></p>"#;
        assert_eq!(unwrap_image_links(html), r#"<p><img src="https://example.com/x.png" alt="art"/></p>"#);
    }

    #[test]
    fn anchor_with_image_and_text_keeps_only_the_image() {
        let html = "<a href=\"/x\">see <img src=\"https://example.com/x.png\"> here</a>";
        assert_eq!(unwrap_image_links(html), "<img src=\"https://example.com/x.png\">");
    }

    #[test]
    fn text_only_anchor_is_left_for_strip_links() {
        let html = "<a href=\"/next\">Next Chapter</a>";
        assert_eq!(unwrap_image_links(html), html);
    }

    #[test]
    fn extract_normalises_tags_strips_query_and_lists_urls_in_order() {
        let html = r#"<p>a</p><img class="x" srcset="a 1x" data-attachment-id="1" src="https://i0.example.com/img/a.png?resize=300%2C200&amp;ssl=1" alt="A &amp; B" width="300"/><p>b</p><img src="https://example.com/b.jpg?w=600">"#;
        let (out, urls) = extract_images(html);
        assert_eq!(urls, vec!["https://i0.example.com/img/a.png", "https://example.com/b.jpg"]);
        assert_eq!(out, r#"<p>a</p><img src="https://i0.example.com/img/a.png" alt="A &amp; B" style="max-width:100%;height:auto"/><p>b</p><img src="https://example.com/b.jpg" alt="" style="max-width:100%;height:auto"/>"#);
    }

    #[test]
    fn protocol_relative_is_made_https_and_site_relative_is_dropped() {
        let html = r#"<img src="//cdn.example.com/a.png"/><img src="/wp-content/b.png"/><img alt="no src"/>"#;
        let (out, urls) = extract_images(html);
        assert_eq!(urls, vec!["https://cdn.example.com/a.png"]);
        assert_eq!(out, r#"<img src="https://cdn.example.com/a.png" alt="" style="max-width:100%;height:auto"/>"#);
    }

    #[test]
    fn duplicate_urls_are_listed_once() {
        let html = r#"<img src="https://example.com/a.png"/><img src="https://example.com/a.png"/>"#;
        let (_, urls) = extract_images(html);
        assert_eq!(urls, vec!["https://example.com/a.png"]);
    }

    #[test]
    fn assign_rewrites_every_tag_for_the_url_and_drop_removes_it() {
        let (html, urls) = extract_images(r#"<img src="https://example.com/a.png"/><img src="https://example.com/a.png"/><img src="https://example.com/b.png"/>"#);
        let out = assign_image_paths(&html, &[(urls[0].clone(), "images/7-0.jpg".into())]);
        assert_eq!(out.matches("src=\"images/7-0.jpg\"").count(), 2);
        let out = drop_image(&out, &urls[1]);
        assert!(!out.contains("b.png"));
        assert_eq!(out.matches("<img").count(), 2);
    }
}
