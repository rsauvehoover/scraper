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
            .map_err(ScrapeError::Http)?;

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

        let resp = request.send().await.map_err(ScrapeError::Http)?;
        let body = resp.text().await.map_err(ScrapeError::Http)?;

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
            db.upsert_chapter_from_toc(&chapter.title, &chapter.uri, volume_id)?;
            *volume_counts
                .entry(chapter.volume_name.clone())
                .or_insert(0) += 1;
        }

        for (volume_name, count) in &volume_counts {
            println!("  Indexed {} with {} chapters", volume_name, count);
        }

        println!("({}) Finished building index", scraper.source_id());
        Ok(())
    }

    /// Download all missing chapters for a source
    /// This function loops until no more chapters are discovered (for paginated TOC sources)
    pub async fn download_all_chapters(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        delay_ms: u64,
    ) -> ScrapeResult<()> {
        let auth_headers = scraper.build_auth_headers();
        let mut total_downloaded = 0;
        let mut total_discovered = 0;

        loop {
            let chapters = db.get_empty_chapters()?;

            if chapters.is_empty() {
                if total_downloaded == 0 {
                    println!("({}) No chapters to download", scraper.source_id());
                }
                break;
            }

            println!(
                "({}) Downloading {} missing chapters",
                scraper.source_id(),
                chapters.len()
            );

            let mut batch_count = 0;
            let mut batch_discovered = 0;

            for chapter in chapters {
                if batch_count % 10 == 0 && batch_count != 0 {
                    println!(
                        "({}) Downloaded {} chapters",
                        scraper.source_id(),
                        batch_count
                    );
                }

                if batch_count > 0 {
                    sleep(Duration::from_millis(delay_ms)).await;
                }

                match self
                    .download_chapter(scraper, db, &chapter, auth_headers.as_ref())
                    .await
                {
                    Ok(discovered) => {
                        batch_count += 1;
                        batch_discovered += discovered;
                    }
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

            total_downloaded += batch_count;
            total_discovered += batch_discovered;

            // If we discovered new chapters, loop again to download them
            if batch_discovered == 0 {
                break;
            }

            println!(
                "({}) Discovered {} new chapters, continuing download...",
                scraper.source_id(),
                batch_discovered
            );
        }

        if total_downloaded > 0 {
            println!(
                "({}) Done downloading {} chapters (discovered {} via next-chapter links)",
                scraper.source_id(),
                total_downloaded,
                total_discovered
            );
        }
        Ok(())
    }

    async fn download_chapter(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        chapter: &crate::db::Chapter,
        auth_headers: Option<&Vec<(String, String)>>,
    ) -> ScrapeResult<usize> {
        let html = self.get_html(&chapter.uri, auth_headers).await?;
        let content = scraper.parse_chapter(&html, &chapter.name).await?;

        if content.is_patreon_only && auth_headers.is_none() {
            println!(
                "({}) Removing Patreon-only chapter: {}",
                scraper.source_id(),
                chapter.name
            );
            db.remove_chapter(chapter.id)?;
            return Ok(0);
        }

        // Apply strip-links post-processor during download
        let processed_html = if scraper
            .post_processors()
            .contains(&"strip-links".to_string())
        {
            let re = regex::Regex::new(r"<a.*?</a>").unwrap();
            re.replace_all(&content.html, "").to_string()
        } else {
            content.html
        };

        db.add_chapter_data(chapter.id, &processed_html)?;

        // Handle chapter discovery: if there's a next chapter URL, add it to the database
        let mut discovered = 0;
        if let Some(next_url) = &content.next_chapter_url {
            // Use the next chapter title if available, otherwise generate a placeholder
            let next_title = content
                .next_chapter_title
                .clone()
                .unwrap_or_else(|| format!("Chapter (discovered from {})", chapter.name));

            // Get the volume name from the current chapter
            let volume_name = db
                .get_volume_name(chapter.volume_id)
                .unwrap_or_else(|_| "Main Story".to_string());

            match db.add_discovered_chapter(&next_title, next_url, &volume_name) {
                Ok(true) => {
                    println!(
                        "({}) Discovered new chapter: {}",
                        scraper.source_id(),
                        next_url
                    );
                    discovered = 1;
                }
                Ok(false) => {} // Chapter already exists
                Err(e) => {
                    println!(
                        "({}) Error adding discovered chapter: {}",
                        scraper.source_id(),
                        e
                    );
                }
            }
        }

        Ok(discovered)
    }
}

impl Default for ScraperClient {
    fn default() -> Self {
        Self::new().expect("Failed to create scraper client")
    }
}
