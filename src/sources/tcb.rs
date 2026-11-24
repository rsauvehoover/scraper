use async_trait::async_trait;
use regex::Regex;
use soup::prelude::*;

use crate::config::{SourceConfig, SourceKind};
use crate::error::{ScrapeError, ScrapeResult};

use super::manga::{chapter_number, compile_chapter_pattern, decode_entities, manga_content, order_chapters, resolve_against_origin, NumberedChapter};
use super::traits::{ChapterContent, ScrapedChapter, SourceScraper};

/// Scraper for TCB-style scan sites: a project page listing `/chapters/...` links,
/// and chapter pages whose pages are `img.fixed-ratio-content`.
pub struct TcbScraper {
    config: SourceConfig,
    /// Compiled `Selectors.ChapterPattern`; an invalid pattern surfaces from `parse_toc`.
    pattern: Result<Regex, String>,
}

impl TcbScraper {
    pub fn new(config: SourceConfig) -> Self {
        let pattern = compile_chapter_pattern(&config.selectors.chapter_pattern);
        TcbScraper { config, pattern }
    }
}

#[async_trait]
impl SourceScraper for TcbScraper {
    fn source_id(&self) -> &str {
        &self.config.id
    }

    fn source_name(&self) -> &str {
        &self.config.name
    }

    fn toc_url(&self) -> &str {
        &self.config.toc_url
    }

    async fn parse_toc(&self, html: &str) -> ScrapeResult<Vec<ScrapedChapter>> {
        let pattern = self.pattern.as_ref().map_err(|e| ScrapeError::Config(e.clone()))?;
        let soup = Soup::new(html);
        let found = soup
            .tag("a")
            .find_all()
            .filter_map(|link| {
                let href = link.get("href")?;
                if !href.contains("/chapters/") {
                    return None;
                }
                let number = chapter_number(&href, pattern)?;
                Some(NumberedChapter {
                    chapter: ScrapedChapter {
                        title: decode_entities(&link.text()),
                        uri: resolve_against_origin(&self.config.toc_url, &href),
                        volume_name: self.config.name.clone(),
                    },
                    number,
                })
            })
            .collect();
        Ok(order_chapters(found, self.config.start_chapter))
    }

    async fn parse_chapter(&self, html: &str, title: &str) -> ScrapeResult<ChapterContent> {
        let soup = Soup::new(html);
        let pages: Vec<String> = soup
            .tag("img")
            .find_all()
            .filter(|img| {
                img.get("class")
                    .is_some_and(|c| c.split_whitespace().any(|t| t == "fixed-ratio-content"))
            })
            .filter_map(|img| img.get("src"))
            .collect();
        manga_content(&self.config, title, pages)
    }

    fn build_auth_headers(&self) -> Option<Vec<(String, String)>> {
        None
    }

    fn author(&self) -> &str {
        &self.config.metadata.author
    }

    fn description(&self) -> &str {
        &self.config.metadata.description
    }

    fn post_processors(&self) -> &[String] {
        &self.config.post_processors
    }

    fn kind(&self) -> SourceKind {
        SourceKind::MangaTcb
    }

    fn volume_ends(&self) -> &[f64] {
        &self.config.volume_ends
    }

    fn chapter_pattern(&self) -> Option<&Regex> {
        self.pattern.as_ref().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SourceConfig, SourceKind};
    use crate::error::ScrapeError;

    fn cfg(start: Option<f64>) -> SourceConfig {
        SourceConfig { id: "tcb-example".into(), name: "Example Series".into(), kind: SourceKind::MangaTcb,
            toc_url: "https://scans.example.com/mangas/5/example-series".into(), start_chapter: start, ..SourceConfig::default() }
    }

    #[tokio::test]
    async fn toc_lists_chapters_oldest_first_resolved_against_the_origin() {
        let toc = TcbScraper::new(cfg(None)).parse_toc(include_str!("../../tests/fixtures/manga/tcb-project.html")).await.unwrap();
        let uris: Vec<&str> = toc.iter().map(|c| c.uri.as_str()).collect();
        assert_eq!(uris, [
            "https://scans.example.com/chapters/1514/example-series-spin-off-by-someone-chapter-1",
            "https://scans.example.com/chapters/100/example-series-chapter-980-review-1688718749",
            "https://scans.example.com/chapters/7990/example-series-chapter-1197",
            "https://scans.example.com/chapters/8001/example-series-chapter-1197.5",
            "https://scans.example.com/chapters/8012/example-series-chapter-1198",
        ]);
        assert_eq!(toc[4].title, "Example Series Chapter 1198");
        assert!(toc.iter().all(|c| c.volume_name == "Example Series"));
    }

    #[tokio::test]
    async fn start_chapter_applies() {
        let toc = TcbScraper::new(cfg(Some(1198.0))).parse_toc(include_str!("../../tests/fixtures/manga/tcb-project.html")).await.unwrap();
        assert_eq!(toc.len(), 1);
        assert!(toc[0].uri.ends_with("chapter-1198"));
    }

    #[tokio::test]
    async fn chapter_pages_are_the_fixed_ratio_images_in_order() {
        let c = TcbScraper::new(cfg(None)).parse_chapter(include_str!("../../tests/fixtures/manga/tcb-chapter.html"), "Example Series Chapter 1198").await.unwrap();
        assert_eq!(c.images, [
            "https://cdn.example.com/file/CDN/ex_1198_pa_01.png",
            "https://cdn.example.com/file/CDN/ex_1198_pa_01v2.png",
            "https://cdn.example.com/file/CDN/ex_1198_pa_02.png",
        ]);
        assert!(!c.html.contains("h-logo.png"));
        assert_eq!(c.html.matches("<div class=\"page\"").count(), 3);
    }

    #[tokio::test]
    async fn a_chapter_page_without_pages_is_not_yet_available() {
        let err = TcbScraper::new(cfg(None)).parse_chapter("<html><body><h1>Soon</h1></body></html>", "X").await.unwrap_err();
        assert!(matches!(err, ScrapeError::NotYetAvailable(_)));
    }

    #[tokio::test]
    async fn a_chapter_pattern_without_a_capture_group_is_a_config_error() {
        let mut c = cfg(None);
        c.selectors.chapter_pattern = r"chapter-\d+".into();
        let err = TcbScraper::new(c).parse_toc(include_str!("../../tests/fixtures/manga/tcb-project.html")).await.unwrap_err();
        assert!(matches!(err, ScrapeError::Config(_)), "{:?}", err);
    }
}
