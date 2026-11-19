use async_trait::async_trait;
use regex::Regex;
use soup::prelude::*;

use crate::config::{Selectors, SourceConfig, SourceKind};
use crate::error::{ScrapeError, ScrapeResult};

use super::manga::{chapter_number, decode_entities, manga_content, order_chapters, resolve_against_origin, NumberedChapter};
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
        let pattern = Regex::new(&config.selectors.chapter_pattern).map_err(|e| {
            format!("invalid chapter pattern '{}': {}", config.selectors.chapter_pattern, e)
        });
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

