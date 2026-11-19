use regex::Regex;

use crate::config::SourceConfig;
use crate::error::{ScrapeError, ScrapeResult};

use super::content_images::attr_escape;
use super::traits::{ChapterContent, ScrapedChapter};

/// A chapter with the number extracted from its URL.
#[derive(Clone, Debug)]
pub struct NumberedChapter {
    pub chapter: ScrapedChapter,
    pub number: f64,
}

/// Compile `Selectors.ChapterPattern`. The error is kept for `parse_toc` to
/// surface as `ScrapeError::Config`. A pattern without a capture group is an
/// error too: `chapter_number` reads group 1, so such a pattern would match
/// every chapter link and still index nothing.
pub fn compile_chapter_pattern(pattern: &str) -> Result<Regex, String> {
    let regex =
        Regex::new(pattern).map_err(|e| format!("invalid chapter pattern '{}': {}", pattern, e))?;
    if regex.captures_len() < 2 {
        return Err(format!(
            "ChapterPattern must have one capture group for the chapter number: {}",
            pattern
        ));
    }
    Ok(regex)
}

/// Extract the chapter number from the first capture group of `pattern`.
pub fn chapter_number(url: &str, pattern: &Regex) -> Option<f64> {
    pattern
        .captures(url)?
        .get(1)?
        .as_str()
        .parse::<f64>()
        .ok()
}

/// Drop chapters below `start`, dedupe by URI (first wins), and sort oldest first.
pub fn order_chapters(found: Vec<NumberedChapter>, start: Option<f64>) -> Vec<ScrapedChapter> {
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<NumberedChapter> = found
        .into_iter()
        .filter(|c| start.is_none_or(|s| c.number >= s))
        .filter(|c| seen.insert(c.chapter.uri.clone()))
        .collect();
    kept.sort_by(|a, b| a.number.total_cmp(&b.number));
    kept.into_iter().map(|c| c.chapter).collect()
}

/// Build the chapter XHTML: the text parsers' header, one page div per image, footer.
pub fn pages_html(source: &SourceConfig, _title: &str, pages: &[String]) -> String {
    let header = format!(
        r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
<meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />
<meta name="author" content="{}"/>
<meta name="description" content="{}"/>
<meta name="classification" content="Fantasy" />
<title>{}</title>
<link rel="stylesheet" href="style.css" type = "text/css" />
</head>
<body>"#,
        source.metadata.author, source.metadata.description, source.name
    );

    let mut html = header;
    html.push('\n');
    for (i, url) in pages.iter().enumerate() {
        html.push_str(&format!(
            r#"<div class="page" style="text-align:center;page-break-after:always"><img src="{}" alt="Page {}" style="max-width:100%;max-height:100%"/></div>"#,
            attr_escape(url),
            i + 1
        ));
        html.push('\n');
    }
    html.push_str("</body></html>\n");
    html
}

/// Build chapter content from page image URLs. No pages means the chapter is not out yet.
pub fn manga_content(
    source: &SourceConfig,
    title: &str,
    pages: Vec<String>,
) -> ScrapeResult<ChapterContent> {
    if pages.is_empty() {
        return Err(ScrapeError::NotYetAvailable(title.to_string()));
    }
    Ok(ChapterContent {
        title: title.to_string(),
        html: pages_html(source, title, &pages),
        is_patreon_only: false,
        next_chapter_url: None,
        next_chapter_title: None,
        images: pages,
    })
}

/// Resolve `href` against the scheme and host of `toc_url`; absolute hrefs pass through.
pub fn resolve_against_origin(toc_url: &str, href: &str) -> String {
    if href.contains("://") {
        return href.to_string();
    }
    let origin = match toc_url.find("://") {
        Some(i) => match toc_url[i + 3..].find('/') {
            Some(j) => &toc_url[..i + 3 + j],
            None => toc_url,
        },
        None => toc_url,
    };
    format!("{}{}", origin, href)
}

/// Decode HTML entities in a title, then collapse whitespace and trim.
pub fn decode_entities(s: &str) -> String {
    let numeric = Regex::new(r"&#(?:[xX]([0-9a-fA-F]+)|([0-9]+));").unwrap();
    let decoded = numeric.replace_all(s, |caps: &regex::Captures| {
        let code = match (caps.get(1), caps.get(2)) {
            (Some(h), _) => u32::from_str_radix(h.as_str(), 16).ok(),
            (_, Some(d)) => d.as_str().parse::<u32>().ok(),
            _ => None,
        };
        code.and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_else(|| caps[0].to_string())
    });
    // &amp; last so "&amp;#8217;" is not decoded twice.
    let decoded = decoded
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re() -> Regex { Regex::new(r"chapter-([0-9]+(?:\.[0-9]+)?)").unwrap() }
    fn nc(uri: &str, n: f64) -> NumberedChapter {
        NumberedChapter { chapter: ScrapedChapter { title: format!("Ch {}", n), uri: uri.into(), volume_name: "S".into() }, number: n }
    }

    #[test]
    fn compile_chapter_pattern_requires_a_capture_group() {
        let err = compile_chapter_pattern(r"chapter-\d+").unwrap_err();
        assert!(err.contains("one capture group") && err.contains(r"chapter-\d+"), "{}", err);
        assert!(compile_chapter_pattern(r"chapter-(\d+").unwrap_err().contains("invalid chapter pattern"));
        assert!(compile_chapter_pattern(r"chapter-(\d+)").is_ok());
    }

    #[test]
    fn number_comes_from_the_first_capture_group() {
        assert_eq!(chapter_number("https://example.com/manga/series-chapter-95/", &re()), Some(95.0));
        assert_eq!(chapter_number("/chapters/8008/series-chapter-391.5", &re()), Some(391.5));
        assert_eq!(chapter_number("https://example.com/manga/other-series/", &re()), None);
    }

    #[test]
    fn start_chapter_drops_lower_numbers_and_keeps_the_boundary_and_decimals() {
        let found = vec![nc("/c-392", 392.0), nc("/c-391.5", 391.5), nc("/c-391", 391.0), nc("/c-390", 390.0)];
        let kept: Vec<f64> = order_chapters(found.clone(), Some(391.0)).iter().map(|c| chapter_number(&c.uri, &Regex::new(r"c-([0-9.]+)").unwrap()).unwrap()).collect();
        assert_eq!(kept, vec![391.0, 391.5, 392.0]);
        let kept: Vec<String> = order_chapters(found, Some(392.0)).iter().map(|c| c.uri.clone()).collect();
        assert_eq!(kept, vec!["/c-392"]);
    }

    #[test]
    fn chapters_are_deduplicated_by_uri_and_ordered_oldest_first() {
        let found = vec![nc("/c-3", 3.0), nc("/c-2", 2.0), nc("/c-3", 3.0), nc("/c-1", 1.0)];
        let uris: Vec<String> = order_chapters(found, None).iter().map(|c| c.uri.clone()).collect();
        assert_eq!(uris, vec!["/c-1", "/c-2", "/c-3"]);
    }

    #[test]
    fn pages_html_has_one_page_div_per_url_and_no_heading() {
        let html = pages_html(&SourceConfig::default(), "Ch 1", &["https://example.com/p/1.png".into(), "https://example.com/p/2.png?x=1&y=2".into()]);
        assert_eq!(html.matches(r#"<div class="page" style="text-align:center;page-break-after:always">"#).count(), 2);
        assert!(html.contains(r#"<img src="https://example.com/p/2.png?x=1&amp;y=2" alt="Page 2" style="max-width:100%;max-height:100%"/>"#));
        assert!(!html.contains("<h1"));
        assert!(html.starts_with("<!DOCTYPE html"));
    }

    #[test]
    fn no_pages_means_not_yet_available() {
        let err = manga_content(&SourceConfig::default(), "Ch 42", vec![]).unwrap_err();
        assert!(matches!(err, ScrapeError::NotYetAvailable(t) if t == "Ch 42"));
        let ok = manga_content(&SourceConfig::default(), "Ch 41", vec!["https://example.com/1.png".into()]).unwrap();
        assert_eq!(ok.images, vec!["https://example.com/1.png"]);
        assert!(!ok.is_patreon_only && ok.next_chapter_url.is_none());
    }

    #[test]
    fn relative_hrefs_resolve_against_the_toc_origin() {
        assert_eq!(resolve_against_origin("https://example.com/a/b", "/chapters/1"), "https://example.com/chapters/1");
        assert_eq!(resolve_against_origin("https://example.com", "/chapters/1"), "https://example.com/chapters/1");
        assert_eq!(resolve_against_origin("https://example.com/a", "https://example.net/x"), "https://example.net/x");
    }

    #[test]
    fn entities_are_decoded_and_whitespace_collapsed() {
        assert_eq!(decode_entities("JoJo&#8217;s Adventure &#8211; Part, Chapter 95"), "JoJo’s Adventure – Part, Chapter 95");
        assert_eq!(decode_entities("  Series  Chapter 1198 "), "Series Chapter 1198");
        assert_eq!(decode_entities("You&#039;re &amp; Next"), "You're & Next");
    }
}
