use async_trait::async_trait;

use crate::config::SourceConfig;
use crate::error::ScrapeResult;

use super::generic::GenericScraper;
use super::traits::{ChapterContent, ScrapedChapter, SourceScraper};

/// Specialized scraper for The Wandering Inn
/// Wraps GenericScraper with any TWI-specific logic
pub struct WanderingInnScraper {
    inner: GenericScraper,
}

impl WanderingInnScraper {
    pub fn new(config: SourceConfig) -> Self {
        WanderingInnScraper {
            inner: GenericScraper::new(config),
        }
    }

    /// Create with default Wandering Inn configuration
    pub fn default_config() -> Self {
        Self::new(SourceConfig::wandering_inn())
    }

    /// Create with Patreon authentication
    pub fn with_patreon(patreon_name: &str) -> Self {
        Self::new(SourceConfig::wandering_inn_with_patreon(patreon_name))
    }
}

#[async_trait]
impl SourceScraper for WanderingInnScraper {
    fn source_id(&self) -> &str {
        self.inner.source_id()
    }

    fn source_name(&self) -> &str {
        self.inner.source_name()
    }

    fn toc_url(&self) -> &str {
        self.inner.toc_url()
    }

    async fn parse_toc(&self, html: &str) -> ScrapeResult<Vec<ScrapedChapter>> {
        self.inner.parse_toc(html).await
    }

    async fn parse_chapter(&self, html: &str, title: &str) -> ScrapeResult<ChapterContent> {
        self.inner.parse_chapter(html, title).await
    }

    fn build_auth_headers(&self) -> Option<Vec<(String, String)>> {
        self.inner.build_auth_headers()
    }

    fn author(&self) -> &str {
        self.inner.author()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn post_processors(&self) -> &[String] {
        self.inner.post_processors()
    }
}
