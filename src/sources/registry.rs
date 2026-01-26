use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{Config, SourceConfig};
use crate::error::{ScrapeError, ScrapeResult};

use super::generic::GenericScraper;
use super::traits::SourceScraper;
use super::wandering_inn::WanderingInnScraper;

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
    fn create_scraper(source: &SourceConfig) -> Arc<dyn SourceScraper> {
        match source.id.as_str() {
            "wandering-inn" => Arc::new(WanderingInnScraper::new(source.clone())),
            _ => Arc::new(GenericScraper::new(source.clone())),
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
    pub fn all(&self) -> impl Iterator<Item = &Arc<dyn SourceScraper>> {
        self.scrapers.values()
    }

    /// Get all source IDs
    pub fn source_ids(&self) -> impl Iterator<Item = &str> {
        self.scrapers.keys().map(|s| s.as_str())
    }

    /// Check if a source is registered
    pub fn has_source(&self, source_id: &str) -> bool {
        self.scrapers.contains_key(source_id)
    }
}

impl Default for ScraperRegistry {
    fn default() -> Self {
        Self::new()
    }
}
