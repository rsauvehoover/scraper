use async_trait::async_trait;

use crate::error::ScrapeResult;

/// Parsed chapter data from TOC
#[derive(Debug, Clone)]
pub struct ScrapedChapter {
    pub title: String,
    pub uri: String,
    pub volume_name: String,
}

/// Parsed chapter content
#[derive(Debug, Clone)]
pub struct ChapterContent {
    pub title: String,
    pub html: String,
    pub is_patreon_only: bool,
}

/// Trait for source scrapers
#[async_trait]
pub trait SourceScraper: Send + Sync {
    /// Get the unique source identifier
    fn source_id(&self) -> &str;

    /// Get the human-readable source name
    fn source_name(&self) -> &str;

    /// Get the table of contents URL
    fn toc_url(&self) -> &str;

    /// Parse the table of contents page and extract chapters
    async fn parse_toc(&self, html: &str) -> ScrapeResult<Vec<ScrapedChapter>>;

    /// Parse a chapter page and extract content
    async fn parse_chapter(&self, html: &str, title: &str) -> ScrapeResult<ChapterContent>;

    /// Build authentication headers/cookies for requests
    fn build_auth_headers(&self) -> Option<Vec<(String, String)>>;

    /// Check if this source requires authentication
    fn requires_auth(&self) -> bool {
        self.build_auth_headers().is_some()
    }

    /// Get the author for EPUB metadata
    fn author(&self) -> &str;

    /// Get the description for EPUB metadata
    fn description(&self) -> &str;

    /// Get the list of post-processor names to apply
    fn post_processors(&self) -> &[String];
}
