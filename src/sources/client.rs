use reqwest::{header::HeaderValue, header::COOKIE, header::USER_AGENT, Client};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::db::SourceDatabase;
use crate::error::{ScrapeError, ScrapeResult};

use super::traits::SourceScraper;

/// HTTP client wrapper for scraping operations
pub struct ScraperClient {
    client: Client,
}

impl ScraperClient {
    /// Create a new scraper client
    pub fn new() -> ScrapeResult<Self> {
        let client = Client::builder()
            .cookie_store(true)
            .build()
            .map_err(ScrapeError::HttpError)?;

        Ok(ScraperClient { client })
    }

    /// Fetch HTML from a URL with optional authentication
    pub async fn get_html(
        &self,
        url: &str,
        auth_headers: Option<&Vec<(String, String)>>,
    ) -> ScrapeResult<String> {
        let mut request = self.client.get(url).header(USER_AGENT, "reqwest");

        if let Some(headers) = auth_headers {
            for (name, value) in headers {
                if name.to_lowercase() == "cookie" {
                    request = request.header(COOKIE, HeaderValue::from_str(value).unwrap());
                }
            }
        }

        let resp = request.send().await.map_err(ScrapeError::HttpError)?;
        let body = resp.text().await.map_err(ScrapeError::HttpError)?;

        Ok(body)
    }

    /// Update the index (table of contents) for a source
    pub async fn update_index(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
    ) -> ScrapeResult<()> {
        println!(
            "({}) Rebuilding index from {}",
            scraper.source_id(),
            scraper.toc_url()
        );

        let html = self.get_html(scraper.toc_url(), None).await?;
        let chapters = scraper.parse_toc(&html).await?;

        let mut volume_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for chapter in chapters {
            let volume_id = db.add_volume(&chapter.volume_name)?;
            db.add_chapter(&chapter.title, &chapter.uri, volume_id)?;
            *volume_counts.entry(chapter.volume_name.clone()).or_insert(0) += 1;
        }

        for (volume_name, count) in &volume_counts {
            println!("  Indexed {} with {} chapters", volume_name, count);
        }

        println!("({}) Finished building index", scraper.source_id());
        Ok(())
    }

    /// Download all missing chapters for a source
    pub async fn download_all_chapters(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        delay_ms: u64,
    ) -> ScrapeResult<()> {
        let chapters = db.get_empty_chapters()?;

        if chapters.is_empty() {
            println!("({}) No chapters to download", scraper.source_id());
            return Ok(());
        }

        println!(
            "({}) Downloading {} missing chapters",
            scraper.source_id(),
            chapters.len()
        );

        let auth_headers = scraper.build_auth_headers();
        let mut count = 0;

        for chapter in chapters {
            if count % 10 == 0 && count != 0 {
                println!("({}) Downloaded {} chapters", scraper.source_id(), count);
            }

            if count > 0 {
                sleep(Duration::from_millis(delay_ms)).await;
            }

            match self
                .download_chapter(scraper, db, &chapter, auth_headers.as_ref())
                .await
            {
                Ok(_) => count += 1,
                Err(e) => {
                    println!(
                        "({}) Error downloading '{}': {}",
                        scraper.source_id(),
                        chapter.name,
                        e
                    );
                }
            }
        }

        println!(
            "({}) Done downloading {} chapters",
            scraper.source_id(),
            count
        );
        Ok(())
    }

    async fn download_chapter(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        chapter: &crate::db::Chapter,
        auth_headers: Option<&Vec<(String, String)>>,
    ) -> ScrapeResult<()> {
        let html = self.get_html(&chapter.uri, auth_headers).await?;
        let content = scraper.parse_chapter(&html, &chapter.name).await?;

        if content.is_patreon_only && auth_headers.is_none() {
            println!(
                "({}) Removing Patreon-only chapter: {}",
                scraper.source_id(),
                chapter.name
            );
            db.remove_chapter(chapter.id)?;
            return Ok(());
        }

        // Apply strip-links post-processor during download
        let processed_html = if scraper.post_processors().contains(&"strip-links".to_string()) {
            let re = regex::Regex::new(r"<a.*?</a>").unwrap();
            re.replace_all(&content.html, "").to_string()
        } else {
            content.html
        };

        db.add_chapter_data(chapter.id, &processed_html)?;
        Ok(())
    }
}

impl Default for ScraperClient {
    fn default() -> Self {
        Self::new().expect("Failed to create scraper client")
    }
}
