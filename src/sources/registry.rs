use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{Config, SourceConfig, SourceKind};
use crate::error::{ScrapeError, ScrapeResult};

use super::generic::GenericScraper;
use super::royal_road::RoyalRoadScraper;
use super::tcb::TcbScraper;
use super::traits::SourceScraper;
use super::wandering_inn::WanderingInnScraper;
use super::wordpress_comic::WordPressComicScraper;

/// Registry for source scrapers
pub struct ScraperRegistry {
    scrapers: HashMap<String, Arc<dyn SourceScraper>>,
}

impl ScraperRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        ScraperRegistry {
            scrapers: HashMap::new(),
        }
    }

    /// Build registry from configuration
    pub fn from_config(config: &Config) -> Self {
        let mut registry = Self::new();

        for source in config.enabled_sources() {
            let scraper = Self::create_scraper(source);
            registry.register(scraper);
        }

        registry
    }

    /// Create a scraper for a source configuration
    pub(crate) fn create_scraper(source: &SourceConfig) -> Arc<dyn SourceScraper> {
        match source.kind {
            SourceKind::MangaTcb => Arc::new(TcbScraper::new(source.clone())),
            SourceKind::MangaWordpress => Arc::new(WordPressComicScraper::new(source.clone())),
            SourceKind::Text => match source.id.as_str() {
                "wandering-inn" => Arc::new(WanderingInnScraper::new(source.clone())),
                id if id.starts_with("royal-road") => {
                    Arc::new(RoyalRoadScraper::new(source.clone()))
                }
                _ => Arc::new(GenericScraper::new(source.clone())),
            },
        }
    }

    /// Register a scraper
    pub fn register(&mut self, scraper: Arc<dyn SourceScraper>) {
        self.scrapers
            .insert(scraper.source_id().to_string(), scraper);
    }

    /// Get a scraper by source ID
    pub fn get(&self, source_id: &str) -> ScrapeResult<Arc<dyn SourceScraper>> {
        self.scrapers
            .get(source_id)
            .cloned()
            .ok_or_else(|| ScrapeError::SourceNotFound(source_id.to_string()))
    }

    /// Get all registered scrapers
    #[allow(dead_code)]
    pub fn all(&self) -> impl Iterator<Item = &Arc<dyn SourceScraper>> {
        self.scrapers.values()
    }

    /// Get all source IDs
    #[allow(dead_code)]
    pub fn source_ids(&self) -> impl Iterator<Item = &str> {
        self.scrapers.keys().map(|s| s.as_str())
    }

    /// Check if a source is registered
    #[allow(dead_code)]
    pub fn has_source(&self, source_id: &str) -> bool {
        self.scrapers.contains_key(source_id)
    }
}

impl Default for ScraperRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_selects_the_manga_scrapers_and_text_keeps_the_id_match() {
        let mut s = SourceConfig::default();
        s.id = "royal-road-1".into();
        assert_eq!(ScraperRegistry::create_scraper(&s).kind(), SourceKind::Text);
        s.kind = SourceKind::MangaTcb;
        assert_eq!(ScraperRegistry::create_scraper(&s).kind(), SourceKind::MangaTcb);
        s.kind = SourceKind::MangaWordpress;
        assert_eq!(ScraperRegistry::create_scraper(&s).kind(), SourceKind::MangaWordpress);
    }
}
