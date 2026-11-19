use async_trait::async_trait;
use regex::Regex;
use soup::prelude::*;

use crate::config::{Selectors, SourceConfig, SourceKind};
use crate::error::{ScrapeError, ScrapeResult};

use super::manga::{chapter_number, compile_chapter_pattern, decode_entities, manga_content, order_chapters, resolve_against_origin, NumberedChapter};
use super::traits::{ChapterContent, ScrapedChapter, SourceScraper};

/// Scraper for WordPress comic sites: a chapter-list widget on the home page, and
/// chapter pages whose pages are `<img>` tags inside the post content.
pub struct WordPressComicScraper {
    config: SourceConfig,
    /// Compiled `Selectors.ChapterPattern`; an invalid pattern surfaces from `parse_toc`.
    pattern: Result<Regex, String>,
}

impl WordPressComicScraper {
    pub fn new(mut config: SourceConfig) -> Self {
        // Swap in this kind's defaults only where the config still holds the generic ones.
        let generic = Selectors::default();
        if config.selectors.chapter_entry == generic.chapter_entry {
            config.selectors.chapter_entry = "ceo_latest_comics_widget".into();
        }
        if config.selectors.main_content == generic.main_content {
            config.selectors.main_content = "entry-content".into();
        }
        let pattern = compile_chapter_pattern(&config.selectors.chapter_pattern);
        WordPressComicScraper { config, pattern }
    }
}

/// True for placeholder and theme images that are not comic pages.
fn is_decoration(url: &str) -> bool {
    url.starts_with("data:")
        || url.contains("/wp-content/themes/")
        || url.contains("/wp-content/uploads/")
}

#[async_trait]
impl SourceScraper for WordPressComicScraper {
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
            .class(self.config.selectors.chapter_entry.as_str())
            .find_all()
            .flat_map(|entry| entry.tag("a").find_all().collect::<Vec<_>>())
            .filter_map(|link| {
                let href = link.get("href")?;
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
            .class(self.config.selectors.main_content.as_str())
            .find_all()
            .next()
            .map(|content| content.tag("img").find_all().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|img| {
                img.get("data-src")
                    .or_else(|| img.get("data-lazy-src"))
                    .or_else(|| img.get("src"))
            })
            .filter(|url| !is_decoration(url))
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
        SourceKind::MangaWordpress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SourceConfig, SourceKind};
    use crate::error::ScrapeError;

    fn cfg(start: Option<f64>) -> SourceConfig {
        SourceConfig { id: "example-part".into(), name: "Example Part".into(), kind: SourceKind::MangaWordpress,
            toc_url: "https://manga.example.com/".into(), start_chapter: start, ..SourceConfig::default() }
    }
    const HOME: &str = include_str!("../../tests/fixtures/manga/wp-home.html");

    #[tokio::test]
    async fn toc_takes_only_pattern_matching_links_from_the_widget_oldest_first() {
        let toc = WordPressComicScraper::new(cfg(None)).parse_toc(HOME).await.unwrap();
        let uris: Vec<&str> = toc.iter().map(|c| c.uri.as_str()).collect();
        assert_eq!(uris, [
            "https://manga.example.com/manga/example-part-chapter-1/",
            "https://manga.example.com/manga/example-part-chapter-2/",
            "https://manga.example.com/manga/example-part-chapter-94/",
            "https://manga.example.com/manga/example-part-chapter-95/",
        ]);
        assert_eq!(toc[3].title, "Example’s Adventure – Part, Chapter 95");
        assert_eq!(toc[3].volume_name, "Example Part");
    }

    #[tokio::test]
    async fn sidebar_links_outside_the_widget_are_ignored_and_start_chapter_applies() {
        let toc = WordPressComicScraper::new(cfg(Some(94.0))).parse_toc(HOME).await.unwrap();
        assert_eq!(toc.len(), 2);
    }

    #[tokio::test]
    async fn chapter_pages_exclude_theme_and_upload_images() {
        let c = WordPressComicScraper::new(cfg(None)).parse_chapter(include_str!("../../tests/fixtures/manga/wp-chapter.html"), "Ch 95").await.unwrap();
        assert_eq!(c.images, ["https://img.example.net/s1600/002.jpg", "https://img.example.net/s1600/003.jpg", "https://img.example.net/s1335/004.webp"]);
    }

    #[tokio::test]
    async fn lazy_loaded_pages_use_data_src() {
        let c = WordPressComicScraper::new(cfg(None)).parse_chapter(include_str!("../../tests/fixtures/manga/wp-lazy.html"), "Ch 1").await.unwrap();
        assert_eq!(c.images, ["https://img.example.net/s1600/001.jpg", "https://img.example.net/s1600/002.jpg"]);
    }

    #[tokio::test]
    async fn placeholder_page_is_not_yet_available() {
        let err = WordPressComicScraper::new(cfg(None)).parse_chapter(include_str!("../../tests/fixtures/manga/wp-placeholder.html"), "Ch 42").await.unwrap_err();
        assert!(matches!(err, ScrapeError::NotYetAvailable(_)));
    }

    #[test]
    fn generic_selector_defaults_are_replaced_but_explicit_values_are_kept() {
        let s = WordPressComicScraper::new(cfg(None));
        assert_eq!((s.config.selectors.chapter_entry.as_str(), s.config.selectors.main_content.as_str()), ("ceo_latest_comics_widget", "entry-content"));
        let mut c = cfg(None);
        c.selectors.main_content = "post-body".into();
        assert_eq!(WordPressComicScraper::new(c).config.selectors.main_content, "post-body");
    }

    #[tokio::test]
    async fn a_chapter_pattern_without_a_capture_group_is_a_config_error() {
        let mut c = cfg(None);
        c.selectors.chapter_pattern = r"chapter-\d+".into();
        let err = WordPressComicScraper::new(c).parse_toc(HOME).await.unwrap_err();
        assert!(matches!(err, ScrapeError::Config(_)), "{:?}", err);
    }
}
