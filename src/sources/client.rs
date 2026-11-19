use reqwest::{header::HeaderValue, header::COOKIE, header::USER_AGENT, Client};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use soup::prelude::*;

use crate::config::{ImagesConfig, SourceKind};
use crate::db::{ChapterImage, RegenerateFlags, SourceDatabase, Volume};
use crate::error::{ScrapeError, ScrapeResult};
use crate::images::{process_image, ProcessedImage};

use super::content_images::{assign_image_paths, drop_image};
use super::traits::SourceScraper;

/// Largest image body `get_bytes` will accept, in bytes.
pub const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;

/// Extract a chapter title from a chapter page, best effort.
/// The TOC upsert corrects the name later, so this only needs to be
/// good enough for the immediately generated epub.
fn extract_page_title(html: &str) -> Option<String> {
    let soup = Soup::new(html);

    // WordPress-style entry title (used by wanderinginn.com)
    let entry_title = soup.tag("h1").find_all().find(|h| {
        h.get("class")
            .map_or(false, |c| c.split_whitespace().any(|c| c == "entry-title"))
    });
    if let Some(h1) = entry_title {
        let title = h1.text().trim().to_string();
        if !title.is_empty() {
            return Some(title);
        }
    }

    // Fall back to <title>, trimmed at the site-name separator
    let page_title = soup.tag("title").find().map(|t| t.text())?;
    let page_title = page_title.trim();
    for sep in [" \u{2013} ", " \u{2014} ", " | ", " - "] {
        if let Some(idx) = page_title.find(sep) {
            let title = page_title[..idx].trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }

    if page_title.is_empty() {
        None
    } else {
        Some(page_title.to_string())
    }
}

/// Derive a placeholder title from the last URL path segment
fn title_from_slug(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .replace('-', " ")
}

/// What to do with a chapter when one of its images cannot be fetched or processed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImagePolicy {
    /// Remove the image from the chapter and keep the rest
    DropFailed,
    /// Fail the whole chapter so it is retried on the next run
    FailChapter,
}

impl ImagePolicy {
    /// Text chapters survive a missing illustration; a manga chapter without
    /// one of its pages is not worth storing.
    pub fn for_kind(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Text => ImagePolicy::DropFailed,
            SourceKind::MangaTcb | SourceKind::MangaWordpress => ImagePolicy::FailChapter,
        }
    }
}

/// An image URL from a chapter and, if fetching and processing worked, the result
#[derive(Debug)]
pub struct FetchedImage {
    pub url: String,
    pub image: Option<ProcessedImage>,
}

/// Chapter HTML pointing at local image paths, with the image rows to store
#[derive(Debug)]
pub struct Assembled {
    pub html: String,
    pub images: Vec<ChapterImage>,
}

/// A fetched and assembled chapter, ready to store
struct FetchedChapter {
    assembled: Assembled,
    is_patreon_only: bool,
    next_chapter_url: Option<String>,
    next_chapter_title: Option<String>,
}

/// Number the fetched images, point the HTML at their local paths, and
/// apply `policy` to the images that failed.
pub fn assemble_chapter(
    chapter_id: isize,
    html: &str,
    fetched: &[FetchedImage],
    policy: ImagePolicy,
) -> ScrapeResult<Assembled> {
    let mut html = html.to_string();
    let mut images = Vec::new();
    let mut assigned = Vec::new();

    for f in fetched {
        match &f.image {
            Some(img) => {
                let seq = images.len() as u32;
                let path = format!("images/{}-{}.{}", chapter_id, seq, img.ext);
                assigned.push((f.url.clone(), path.clone()));
                images.push(ChapterImage {
                    chapter_id,
                    seq,
                    path,
                    source_url: f.url.clone(),
                    mime: img.mime.to_string(),
                    width: img.width,
                    height: img.height,
                    data: img.bytes.clone(),
                });
            }
            None => match policy {
                ImagePolicy::DropFailed => html = drop_image(&html, &f.url),
                ImagePolicy::FailChapter => return Err(ScrapeError::Image(f.url.clone())),
            },
        }
    }

    Ok(Assembled {
        html: assign_image_paths(&html, &assigned),
        images,
    })
}

/// Bytes of page images one manga batch may hold: the mail attachment limit
/// less 1 MiB for the EPUB's text, styles and cover.
pub fn batch_budget_bytes(max_attachment_bytes: usize) -> u64 {
    (max_attachment_bytes as u64).saturating_sub(1024 * 1024)
}

/// Where `place_in_batch` put a chapter, and the batch it closed to do so
#[derive(Debug, PartialEq, Eq)]
pub struct Placement {
    pub volume_id: isize,
    pub closed: Option<isize>,
}

/// The batch new manga chapters join: the latest volume, or "Volume 1" when
/// there is none.
fn open_batch(db: &SourceDatabase) -> rusqlite::Result<Volume> {
    match db.get_latest_volume()? {
        Some(volume) => Ok(volume),
        None => Ok(Volume { id: db.add_volume("Volume 1")?, name: "Volume 1".to_string() }),
    }
}

/// Create the batch after `open`, named `Volume n` for the first unused
/// `n >= count + 1`. A name taken earlier (say by `--pull-chapter --volume`)
/// is skipped, so a closed batch is never reused and never grows.
fn new_batch(db: &SourceDatabase, open: isize) -> rusqlite::Result<isize> {
    let mut n = db.count_volumes()? + 1;
    while db.volume_exists(&format!("Volume {}", n))? {
        n += 1;
    }
    let id = db.add_volume(&format!("Volume {}", n))?;
    if id <= open {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!("new batch Volume {} reused volume {} at or before the open batch {}", n, id, open)),
        ));
    }
    Ok(id)
}

/// Move a manga chapter about to be stored into the open batch, or into a new
/// batch when its `this_bytes` of pages would push the open one over `budget`.
/// A batch's first chapter always stays, so an oversized chapter is a batch of one.
pub fn place_in_batch(
    db: &SourceDatabase,
    chapter_id: isize,
    this_bytes: u64,
    budget: u64,
) -> rusqlite::Result<Placement> {
    let open = open_batch(db)?.id;
    let full = db.volume_has_downloaded_chapter(open)?
        && db.volume_image_bytes(open)? + this_bytes > budget;
    let placement = if full {
        Placement { volume_id: new_batch(db, open)?, closed: Some(open) }
    } else {
        Placement { volume_id: open, closed: None }
    };
    db.set_chapter_volume(chapter_id, placement.volume_id)?;
    Ok(placement)
}

/// Place a downloaded manga chapter in its batch and store it. Only the batch
/// it closed, if any, is flagged for EPUB generation; the open batch never is.
fn store_in_batch(
    db: &SourceDatabase,
    chapter_id: isize,
    assembled: &Assembled,
    budget: u64,
) -> rusqlite::Result<()> {
    let this_bytes: u64 = assembled.images.iter().map(|i| i.data.len() as u64).sum();
    // Place and flag in one transaction, committed before storing: if the
    // store then fails, the chapter waits in the new batch and no later run
    // would close the old one.
    let tx = db.connection().unchecked_transaction()?;
    let placement = place_in_batch(db, chapter_id, this_bytes, budget)?;
    if let Some(closed) = placement.closed {
        db.update_generated_volume(closed, true)?;
    }
    tx.commit()?;
    db.store_chapter_content(chapter_id, &assembled.html, &assembled.images, RegenerateFlags::ChapterOnly)
}

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

    /// Fetch HTML like get_html, but fail on non-success HTTP status.
    /// Used by the seed path, where the URL is user-typed.
    async fn get_html_checked(
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
        let resp = resp.error_for_status().map_err(ScrapeError::Http)?;
        let body = resp.text().await.map_err(ScrapeError::Http)?;
        Ok(body)
    }

    /// Fetch raw image bytes from a URL, failing on non-success HTTP status.
    ///
    /// Images come from third-party hosts during unattended runs, so the
    /// request has a timeout and a body over `MAX_IMAGE_BYTES` is refused.
    pub async fn get_bytes(&self, url: &str) -> ScrapeResult<Vec<u8>> {
        let resp = self
            .client
            .get(url)
            .header(USER_AGENT, "reqwest")
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(ScrapeError::Http)?;
        let resp = resp.error_for_status().map_err(ScrapeError::Http)?;
        let too_big = || {
            ScrapeError::Image(format!(
                "{} exceeds the {} byte image limit",
                url, MAX_IMAGE_BYTES
            ))
        };
        if resp.content_length().is_some_and(|len| len > MAX_IMAGE_BYTES as u64) {
            return Err(too_big());
        }
        let bytes = resp.bytes().await.map_err(ScrapeError::Http)?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(too_big());
        }
        Ok(bytes.to_vec())
    }

    /// Fetch and process one image
    async fn fetch_image(&self, url: &str, images: &ImagesConfig) -> ScrapeResult<ProcessedImage> {
        let bytes = self.get_bytes(url).await?;
        let config = images.clone();
        tokio::task::spawn_blocking(move || process_image(&bytes, &config))
            .await
            .map_err(|e| ScrapeError::Image(e.to_string()))?
    }

    /// Fetch and process every image URL, in order. A failure is reported
    /// and recorded as `image: None`; the caller decides what it means.
    pub async fn fetch_images(&self, urls: &[String], images: &ImagesConfig) -> Vec<FetchedImage> {
        let mut fetched = Vec::with_capacity(urls.len());
        for (i, url) in urls.iter().enumerate() {
            if i > 0 {
                sleep(Duration::from_millis(images.request_delay)).await;
            }
            let image = match self.fetch_image(url, images).await {
                Ok(image) => Some(image),
                Err(e) => {
                    println!("  Warning: could not fetch image {}: {}", url, e);
                    None
                }
            };
            fetched.push(FetchedImage { url: url.clone(), image });
        }
        fetched
    }

    /// Update the index (table of contents) for a source
    pub async fn update_index(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        ignored_volumes: &Vec<String>,
        ignored_chapters: &Vec<String>,
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

        // Manga volumes are size-bounded batches, placed at download time:
        // a new chapter waits in the open batch and a known one keeps its volume.
        let open_manga_batch = match scraper.kind() {
            SourceKind::Text => None,
            SourceKind::MangaTcb | SourceKind::MangaWordpress => Some(open_batch(db)?),
        };

        for chapter in chapters {
            if ignored_volumes.contains(&chapter.volume_name) || ignored_chapters.contains(&chapter.title) {
                continue
            }
            let volume_name = match &open_manga_batch {
                Some(open) => {
                    db.upsert_chapter_keep_volume(&chapter.title, &chapter.uri, open.id)?;
                    open.name.clone()
                }
                None => {
                    let volume_id = db.add_volume(&chapter.volume_name)?;
                    db.upsert_chapter_from_toc(&chapter.title, &chapter.uri, volume_id)?;
                    chapter.volume_name
                }
            };
            *volume_counts.entry(volume_name).or_insert(0) += 1;
        }

        for (volume_name, count) in &volume_counts {
            println!("  Indexed {} with {} chapters", volume_name, count);
        }

        println!("({}) Finished building index", scraper.source_id());
        Ok(())
    }

    /// Seed a manually pulled chapter into the database ahead of the TOC.
    /// The normal download/epub/mail pipeline picks it up afterwards; when
    /// the TOC later lists the chapter, upsert_chapter_from_toc matches it
    /// by URI so it is never duplicated or re-sent.
    pub async fn seed_chapter(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        url: &str,
        title_override: Option<&str>,
        volume_override: Option<&str>,
    ) -> ScrapeResult<()> {
        if db.chapter_exists_by_uri(url)? {
            println!(
                "({}) Chapter already in database, skipping: {}",
                scraper.source_id(),
                url
            );
            return Ok(());
        }

        // Always fetch, even when a title override is supplied: this is the
        // only validation that a user-typed URL actually resolves before we
        // seed it into the download/epub/mail pipeline.
        let html = self
            .get_html_checked(url, scraper.build_auth_headers().as_ref())
            .await?;

        let title = match title_override {
            Some(t) => t.to_string(),
            None => extract_page_title(&html).unwrap_or_else(|| title_from_slug(url)),
        };

        // A manga chapter waits in the open batch like any newly indexed one;
        // a named volume could reopen a closed batch.
        let (volume_id, volume_name) = if scraper.kind() != SourceKind::Text {
            if volume_override.is_some() {
                println!(
                    "({}) --volume is ignored for manga sources; seeding into the open batch",
                    scraper.source_id()
                );
            }
            let open = open_batch(db)?;
            (open.id, open.name)
        } else {
            let volume_name = match volume_override {
                Some(v) => v.to_string(),
                None => db
                    .get_latest_volume()?
                    .map(|v| v.name)
                    .unwrap_or_else(|| "Main Story".to_string()),
            };
            (db.add_volume(&volume_name)?, volume_name)
        };

        db.add_chapter(&title, url, volume_id)?;
        println!(
            "({}) Seeded chapter '{}' into {} ({})",
            scraper.source_id(),
            title,
            volume_name,
            url
        );
        Ok(())
    }

    /// Download all missing chapters for a source
    /// This function loops until no more chapters are discovered (for paginated TOC sources)
    pub async fn download_all_chapters(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        delay_ms: u64,
        images: &ImagesConfig,
        max_attachment_bytes: usize,
    ) -> ScrapeResult<()> {
        let auth_headers = scraper.build_auth_headers();
        let budget = batch_budget_bytes(max_attachment_bytes);
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
                    .download_chapter(scraper, db, &chapter, auth_headers.as_ref(), images, budget)
                    .await
                {
                    Ok(discovered) => {
                        batch_count += 1;
                        batch_discovered += discovered;
                    }
                    Err(ScrapeError::NotYetAvailable(title)) => {
                        println!(
                            "({}) Not yet available, will retry: {}",
                            scraper.source_id(),
                            title
                        );
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

    /// Re-fetch every stored chapter and replace its HTML and images without
    /// flagging anything for EPUB generation, so nothing is re-sent.
    /// A failed chapter keeps its old content; a Patreon-only page without
    /// auth is skipped, never removed.
    pub async fn redownload_all(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        delay_ms: u64,
        images: &ImagesConfig,
    ) -> ScrapeResult<()> {
        let auth_headers = scraper.build_auth_headers();
        let chapters = db.get_downloaded_chapters()?;
        println!(
            "({}) Re-downloading {} stored chapters",
            scraper.source_id(),
            chapters.len()
        );

        let mut stored = 0;
        for (i, chapter) in chapters.iter().enumerate() {
            if i > 0 {
                sleep(Duration::from_millis(delay_ms)).await;
            }
            // Checked fetch: an error status must not overwrite stored content
            let fetched = match self.get_html_checked(&chapter.uri, auth_headers.as_ref()).await {
                Ok(html) => {
                    self.fetch_chapter(scraper, chapter, &html, auth_headers.as_ref(), images)
                        .await
                }
                Err(e) => Err(e),
            };
            let result = match fetched {
                Ok(f) if f.is_patreon_only && auth_headers.is_none() => {
                    println!(
                        "({}) Skipping Patreon-only chapter: {}",
                        scraper.source_id(),
                        chapter.name
                    );
                    continue;
                }
                Ok(f) => db
                    .store_chapter_content(chapter.id, &f.assembled.html, &f.assembled.images, RegenerateFlags::Untouched)
                    .map_err(ScrapeError::from),
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => stored += 1,
                Err(ScrapeError::NotYetAvailable(title)) => println!(
                    "({}) Not yet available, will retry: {}",
                    scraper.source_id(),
                    title
                ),
                Err(e) => println!(
                    "({}) Error re-downloading '{}': {}",
                    scraper.source_id(),
                    chapter.name,
                    e
                ),
            }
        }

        println!(
            "({}) Re-downloaded {} of {} chapters",
            scraper.source_id(),
            stored,
            chapters.len()
        );
        Ok(())
    }

    /// Parse an already-fetched chapter page, strip links, fetch images and
    /// assemble. The caller fetches the page so it can choose whether an
    /// error status counts as a failure.
    /// A Patreon-only page without auth is returned unassembled: images are
    /// not fetched and `assembled` is empty.
    async fn fetch_chapter(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        chapter: &crate::db::Chapter,
        html: &str,
        auth_headers: Option<&Vec<(String, String)>>,
        images: &ImagesConfig,
    ) -> ScrapeResult<FetchedChapter> {
        let content = scraper.parse_chapter(html, &chapter.name).await?;

        if content.is_patreon_only && auth_headers.is_none() {
            return Ok(FetchedChapter {
                assembled: Assembled { html: String::new(), images: Vec::new() },
                is_patreon_only: true,
                next_chapter_url: content.next_chapter_url,
                next_chapter_title: content.next_chapter_title,
            });
        }

        // Apply strip-links post-processor during download
        let processed_html = if scraper
            .post_processors()
            .contains(&"strip-links".to_string())
        {
            let re = regex::Regex::new(r"<a\b.*?</a>").unwrap();
            re.replace_all(&content.html, "").to_string()
        } else {
            content.html
        };

        let fetched = self.fetch_images(&content.images, images).await;
        let assembled = assemble_chapter(
            chapter.id,
            &processed_html,
            &fetched,
            ImagePolicy::for_kind(scraper.kind()),
        )?;

        Ok(FetchedChapter {
            assembled,
            is_patreon_only: content.is_patreon_only,
            next_chapter_url: content.next_chapter_url,
            next_chapter_title: content.next_chapter_title,
        })
    }

    async fn download_chapter(
        &self,
        scraper: &Arc<dyn SourceScraper>,
        db: &SourceDatabase,
        chapter: &crate::db::Chapter,
        auth_headers: Option<&Vec<(String, String)>>,
        images: &ImagesConfig,
        batch_budget: u64,
    ) -> ScrapeResult<usize> {
        let html = self.get_html(&chapter.uri, auth_headers).await?;
        let content = self
            .fetch_chapter(scraper, chapter, &html, auth_headers, images)
            .await?;

        if content.is_patreon_only && auth_headers.is_none() {
            println!(
                "({}) Removing Patreon-only chapter: {}",
                scraper.source_id(),
                chapter.name
            );
            db.remove_chapter(chapter.id)?;
            return Ok(0);
        }

        let assembled = content.assembled;
        match scraper.kind() {
            SourceKind::Text => db.store_chapter_content(chapter.id, &assembled.html, &assembled.images, RegenerateFlags::ChapterAndVolume)?,
            SourceKind::MangaTcb | SourceKind::MangaWordpress => {
                store_in_batch(db, chapter.id, &assembled, batch_budget)?
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SourceConfig;
    use crate::sources::generic::GenericScraper;
    use crate::sources::wordpress_comic::WordPressComicScraper;
    use crate::test_support::{serve_fixtures, FixtureRoute};

    /// The default attachment limit, for tests that do not exercise batching
    const LIMIT: usize = 17 * 1024 * 1024;

    fn fetched(url: &str, ok: bool) -> FetchedImage {
        FetchedImage { url: url.into(), image: ok.then(|| ProcessedImage { bytes: vec![1, 2], mime: "image/jpeg", ext: "jpg", width: 1, height: 1 }) }
    }

    fn png_2x2() -> Vec<u8> {
        use image::ImageEncoder;
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([200, 10, 10]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 2, 2, image::ColorType::Rgb8)
            .unwrap();
        out
    }

    #[test]
    fn assemble_assigns_sequential_paths_and_rewrites_html() {
        let html = r#"<img src="https://example.com/a.png" alt="" style="s"/><img src="https://example.com/b.png" alt="" style="s"/>"#;
        let out = assemble_chapter(7, html, &[fetched("https://example.com/a.png", true), fetched("https://example.com/b.png", true)], ImagePolicy::DropFailed).unwrap();
        assert_eq!(out.images.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(), ["images/7-0.jpg", "images/7-1.jpg"]);
        assert_eq!(out.images[1].source_url, "https://example.com/b.png");
        assert!(out.html.contains("src=\"images/7-0.jpg\"") && out.html.contains("src=\"images/7-1.jpg\""));
        assert!(!out.html.contains("example.com"));
    }

    #[test]
    fn text_policy_drops_a_failed_image_and_keeps_the_chapter() {
        let html = r#"<p>t</p><img src="https://example.com/a.png" alt="" style="s"/><img src="https://example.com/b.png" alt="" style="s"/>"#;
        let out = assemble_chapter(7, html, &[fetched("https://example.com/a.png", false), fetched("https://example.com/b.png", true)], ImagePolicy::DropFailed).unwrap();
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].path, "images/7-0.jpg");
        assert_eq!(out.html.matches("<img").count(), 1);
    }

    #[test]
    fn manga_policy_fails_the_chapter_on_a_failed_image() {
        let html = r#"<img src="https://example.com/a.png" alt="" style="s"/>"#;
        let err = assemble_chapter(7, html, &[fetched("https://example.com/a.png", false)], ImagePolicy::FailChapter).unwrap_err();
        assert!(matches!(err, ScrapeError::Image(_)));
    }

    #[test]
    fn policy_follows_kind() {
        assert_eq!(ImagePolicy::for_kind(SourceKind::Text), ImagePolicy::DropFailed);
        assert_eq!(ImagePolicy::for_kind(SourceKind::MangaTcb), ImagePolicy::FailChapter);
        assert_eq!(ImagePolicy::for_kind(SourceKind::MangaWordpress), ImagePolicy::FailChapter);
    }

    /// End to end against a local server: a text chapter with one good image,
    /// one that 404s, and one whose body is HTML.
    #[tokio::test]
    async fn download_stores_chapter_with_fetched_images_and_drops_bad_ones() {
        let png = png_2x2();
        let base = serve_fixtures(|b| vec![
            FixtureRoute { path: "/c1", status: 200, content_type: "text/html", body: format!(r#"<html><head><title>T</title></head><body><div id="content"><p>x</p><img src="{b}/good.png"/><img src="{b}/missing.png"/><img src="{b}/page.html"/></div></body></html>"#, b = b).into_bytes() },
            FixtureRoute { path: "/good.png", status: 200, content_type: "image/png", body: png.clone() },
            FixtureRoute { path: "/page.html", status: 200, content_type: "text/html", body: b"<html/>".to_vec() },
        ]).await;

        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume("V").unwrap();
        db.add_chapter("C1", &format!("{}/c1", base), vol).unwrap();
        let mut cfg = SourceConfig::default();
        cfg.selectors.main_content = "content".into();
        let scraper: Arc<dyn SourceScraper> = Arc::new(GenericScraper::new(cfg));
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().download_all_chapters(&scraper, &db, 0, &images, LIMIT).await.unwrap();

        let ch = db.get_downloaded_chapters().unwrap();
        assert_eq!(ch.len(), 1);
        let imgs = db.get_chapter_images(ch[0].id).unwrap();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].path, format!("images/{}-0.jpg", ch[0].id));
        let html = db.get_chapter_data(ch[0].id).unwrap();
        assert_eq!(html.matches("<img").count(), 1);
        assert!(html.contains(&imgs[0].path));
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_bytes_refuses_a_body_over_the_cap() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: "/big.png", status: 200, content_type: "image/png", body: vec![0u8; MAX_IMAGE_BYTES + 1] },
        ]).await;

        let result = ScraperClient::new().unwrap().get_bytes(&format!("{}/big.png", base)).await;

        assert!(matches!(result, Err(ScrapeError::Image(_))), "got {:?}", result.map(|b| b.len()));
    }

    /// A text-source scraper reading `<div id="content">`, with no auth
    fn content_scraper() -> Arc<dyn SourceScraper> {
        let mut cfg = SourceConfig::default();
        cfg.selectors.main_content = "content".into();
        Arc::new(GenericScraper::new(cfg))
    }

    /// One chapter at `{base}/c1` stored with "<old/>" and both regenerate flags cleared
    fn db_with_stored_chapter(base: &str) -> (SourceDatabase, isize) {
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume("V").unwrap();
        db.add_chapter("C1", &format!("{}/c1", base), vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.add_chapter_data(id, "<old/>").unwrap();
        db.update_generated_chapter(id, false).unwrap();
        db.update_generated_volume(vol, false).unwrap();
        (db, id)
    }

    #[tokio::test]
    async fn redownload_replaces_content_and_images_without_flagging() {
        let png = png_2x2();
        let base = serve_fixtures(|b| vec![
            FixtureRoute { path: "/c1", status: 200, content_type: "text/html", body: format!(r#"<html><head><title>T</title></head><body><div id="content"><p>x</p><img src="{b}/good.png"/></div></body></html>"#, b = b).into_bytes() },
            FixtureRoute { path: "/good.png", status: 200, content_type: "image/png", body: png.clone() },
        ]).await;
        let (db, id) = db_with_stored_chapter(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().redownload_all(&content_scraper(), &db, 0, &images).await.unwrap();

        assert!(db.get_chapter_data(id).unwrap().contains(&format!("images/{}-0.jpg", id)));
        assert_eq!(db.get_chapter_images(id).unwrap().len(), 1);
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }

    #[tokio::test]
    async fn redownload_keeps_old_content_when_the_page_fails() {
        let base = serve_fixtures(|_| vec![]).await;
        let (db, id) = db_with_stored_chapter(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().redownload_all(&content_scraper(), &db, 0, &images).await.unwrap();

        assert_eq!(db.get_chapter_data(id).unwrap(), "<old/>");
    }

    #[tokio::test]
    async fn redownload_keeps_old_content_on_an_error_status_with_a_content_body() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: "/c1", status: 500, content_type: "text/html", body: br#"<html><head><title>E</title></head><body><div id="content"><p>oops</p></div></body></html>"#.to_vec() },
        ]).await;
        let (db, id) = db_with_stored_chapter(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().redownload_all(&content_scraper(), &db, 0, &images).await.unwrap();

        assert_eq!(db.get_chapter_data(id).unwrap(), "<old/>");
        assert!(db.get_chapter_images(id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn redownload_skips_a_patreon_only_chapter_instead_of_deleting_it() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: "/c1", status: 200, content_type: "text/html", body: "<html><head><title>Patron Early Access \u{2013} X</title></head><body><div id=\"content\"><p>locked</p></div></body></html>".as_bytes().to_vec() },
        ]).await;
        let (db, id) = db_with_stored_chapter(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().redownload_all(&content_scraper(), &db, 0, &images).await.unwrap();

        let chapters = db.get_downloaded_chapters().unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].id, id);
        assert_eq!(db.get_chapter_data(id).unwrap(), "<old/>");
    }

    const MANGA_CHAPTER_PATH: &str = "/manga/example-chapter-42/";

    /// A WordPress comic scraper rooted at `base`, and a db with one pending chapter
    /// at `{base}/manga/example-chapter-42/`
    fn manga_setup(base: &str) -> (Arc<dyn SourceScraper>, SourceDatabase) {
        let scraper = manga_scraper(base);
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume("V").unwrap();
        db.add_chapter("Ch 42", &format!("{}{}", base, MANGA_CHAPTER_PATH), vol).unwrap();
        (scraper, db)
    }

    /// A WordPress comic scraper whose TOC is `toc_url`
    fn manga_scraper(toc_url: &str) -> Arc<dyn SourceScraper> {
        let cfg = SourceConfig {
            id: "example-manga".into(),
            kind: SourceKind::MangaWordpress,
            toc_url: toc_url.into(),
            ..SourceConfig::default()
        };
        Arc::new(WordPressComicScraper::new(cfg))
    }

    /// The wp-chapter fixture with its three page URLs pointed at `{base}/p1.png` .. `p3.png`
    fn wp_chapter_on(base: &str) -> Vec<u8> {
        let html = include_str!("../../tests/fixtures/manga/wp-chapter.html")
            .replace("https://img.example.net/s1600/002.jpg", &format!("{}/p1.png", base))
            .replace("https://img.example.net/s1600/003.jpg", &format!("{}/p2.png", base))
            .replace("https://img.example.net/s1335/004.webp", &format!("{}/p3.png", base));
        assert!(!html.contains("img.example.net"), "fixture page URLs changed; update the rewrite");
        html.into_bytes()
    }

    #[tokio::test]
    async fn a_placeholder_manga_chapter_stores_nothing_and_stays_pending() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: MANGA_CHAPTER_PATH, status: 200, content_type: "text/html", body: include_bytes!("../../tests/fixtures/manga/wp-placeholder.html").to_vec() },
        ]).await;
        let (scraper, db) = manga_setup(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        let result = ScraperClient::new().unwrap().download_all_chapters(&scraper, &db, 0, &images, LIMIT).await;

        assert!(result.is_ok());
        assert_eq!(db.get_empty_chapters().unwrap().len(), 1);
        assert!(db.get_downloaded_chapters().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_manga_chapter_with_a_failed_page_stores_nothing() {
        let png = png_2x2();
        let base = serve_fixtures(|b| vec![
            FixtureRoute { path: MANGA_CHAPTER_PATH, status: 200, content_type: "text/html", body: wp_chapter_on(b) },
            FixtureRoute { path: "/p1.png", status: 200, content_type: "image/png", body: png.clone() },
            FixtureRoute { path: "/p3.png", status: 200, content_type: "image/png", body: png.clone() },
        ]).await;
        let (scraper, db) = manga_setup(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().download_all_chapters(&scraper, &db, 0, &images, LIMIT).await.unwrap();

        let pending = db.get_empty_chapters().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(db.get_downloaded_chapters().unwrap().is_empty());
        assert!(db.get_chapter_images(pending[0].id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_manga_chapter_with_all_pages_stores_pages_in_order() {
        let png = png_2x2();
        let base = serve_fixtures(|b| vec![
            FixtureRoute { path: MANGA_CHAPTER_PATH, status: 200, content_type: "text/html", body: wp_chapter_on(b) },
            FixtureRoute { path: "/p1.png", status: 200, content_type: "image/png", body: png.clone() },
            FixtureRoute { path: "/p2.png", status: 200, content_type: "image/png", body: png.clone() },
            FixtureRoute { path: "/p3.png", status: 200, content_type: "image/png", body: png.clone() },
        ]).await;
        let (scraper, db) = manga_setup(&base);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().download_all_chapters(&scraper, &db, 0, &images, LIMIT).await.unwrap();

        let ch = db.get_downloaded_chapters().unwrap();
        assert_eq!(ch.len(), 1);
        let id = ch[0].id;
        let imgs = db.get_chapter_images(id).unwrap();
        assert_eq!(imgs.iter().map(|i| i.seq).collect::<Vec<_>>(), [0, 1, 2]);
        let paths: Vec<String> = (0..3).map(|n| format!("images/{}-{}.jpg", id, n)).collect();
        assert_eq!(imgs.iter().map(|i| i.path.clone()).collect::<Vec<_>>(), paths);
        assert_eq!(
            imgs.iter().map(|i| i.source_url.clone()).collect::<Vec<_>>(),
            ["p1.png", "p2.png", "p3.png"].map(|p| format!("{}/{}", base, p))
        );
        let html = db.get_chapter_data(id).unwrap();
        for p in &paths {
            assert!(html.contains(p.as_str()), "html missing {}", p);
        }
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 1);
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty(), "the open batch is never flagged");
    }

    #[test]
    fn budget_is_the_limit_less_one_mib_saturating() {
        assert_eq!(batch_budget_bytes(17 * 1024 * 1024), 16 * 1024 * 1024);
        assert_eq!(batch_budget_bytes(0), 0);
        assert_eq!(batch_budget_bytes(1024), 0);
    }

    /// Chapters in id order, each with the name of its volume
    fn volume_of_each(db: &SourceDatabase) -> Vec<(String, String)> {
        let mut chapters = db.get_empty_chapters().unwrap();
        chapters.extend(db.get_downloaded_chapters().unwrap());
        chapters.sort_by_key(|c| c.id);
        chapters.into_iter().map(|c| (c.name, db.get_volume_name(c.volume_id).unwrap())).collect()
    }

    fn flagged_volumes(db: &SourceDatabase) -> Vec<String> {
        let mut names: Vec<String> = db.get_volumes_to_regenerate().unwrap().into_iter().map(|v| v.name).collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn manga_index_parks_new_chapters_in_the_open_batch_and_keeps_existing_ones() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: "/", status: 200, content_type: "text/html", body: include_bytes!("../../tests/fixtures/manga/wp-home.html").to_vec() },
        ]).await;
        let scraper = manga_scraper(&format!("{}/", base));
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let v1 = db.add_volume("Volume 1").unwrap();
        db.add_volume("Volume 2").unwrap();
        db.add_chapter("Old name", "https://manga.example.com/manga/example-part-chapter-1/", v1).unwrap();

        ScraperClient::new().unwrap().update_index(&scraper, &db, &vec![], &vec![]).await.unwrap();

        let in_vol = |n: u32, v: &str| (format!("Example\u{2019}s Adventure \u{2013} Part, Chapter {}", n), v.to_string());
        assert_eq!(volume_of_each(&db), [in_vol(1, "Volume 1"), in_vol(2, "Volume 2"), in_vol(94, "Volume 2"), in_vol(95, "Volume 2")]);
        assert_eq!(db.count_volumes().unwrap(), 2, "the index creates no source-named volume");
    }

    #[tokio::test]
    async fn text_index_still_follows_the_toc_volume() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: "/toc", status: 200, content_type: "text/html", body: br#"<html><body>
                <div class="volume-wrapper"><h2>Volume 1</h2><div class="chapter-entry"><a href="https://example.com/c0">C0</a></div></div>
                <div class="volume-wrapper"><h2>Volume 2</h2><div class="chapter-entry"><a href="https://example.com/c1">C1</a></div></div>
                </body></html>"#.to_vec() },
        ]).await;
        let mut cfg = SourceConfig::default();
        cfg.toc_url = format!("{}/toc", base);
        let scraper: Arc<dyn SourceScraper> = Arc::new(GenericScraper::new(cfg));
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let v1 = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", v1).unwrap();

        ScraperClient::new().unwrap().update_index(&scraper, &db, &vec![], &vec![]).await.unwrap();

        let in_vol = |c: &str, v: &str| (c.to_string(), v.to_string());
        assert_eq!(volume_of_each(&db), [in_vol("C1", "Volume 2"), in_vol("C0", "Volume 1")]);
    }

    const BATCH_CHAPTER_PATHS: [&str; 5] = [
        "/manga/example-chapter-1/",
        "/manga/example-chapter-2/",
        "/manga/example-chapter-3/",
        "/manga/example-chapter-4/",
        "/manga/example-chapter-5/",
    ];

    /// Pages per chapter: the wp-chapter fixture's p1..p3
    const PAGES_PER_CHAPTER: u64 = 3;

    /// A 64x64 PNG of deterministic noise, so its stored JPEG is a few KB
    fn png_noisy_64() -> Vec<u8> {
        use image::ImageEncoder;
        let mut state: u32 = 12345;
        let mut next = move || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        };
        let img = image::RgbImage::from_fn(64, 64, |_, _| image::Rgb([next(), next(), next()]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 64, 64, image::ColorType::Rgb8)
            .unwrap();
        out
    }

    /// Stored bytes of one chapter of noisy pages
    fn chapter_bytes() -> u64 {
        let page = process_image(&png_noisy_64(), &ImagesConfig::default()).unwrap().bytes.len() as u64;
        PAGES_PER_CHAPTER * page
    }

    /// Serve the first `n` batch chapters, all with the same three noisy pages,
    /// and return a manga scraper and a db holding them pending in `volume`
    async fn batch_setup(n: usize, volume: &str) -> (Arc<dyn SourceScraper>, SourceDatabase) {
        let png = png_noisy_64();
        let base = serve_fixtures(|b| {
            let mut routes: Vec<FixtureRoute> = BATCH_CHAPTER_PATHS[..n].iter()
                .map(|p| FixtureRoute { path: p, status: 200, content_type: "text/html", body: wp_chapter_on(b) })
                .collect();
            for p in ["/p1.png", "/p2.png", "/p3.png"] {
                routes.push(FixtureRoute { path: p, status: 200, content_type: "image/png", body: png.clone() });
            }
            routes
        }).await;
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume(volume).unwrap();
        for (i, p) in BATCH_CHAPTER_PATHS[..n].iter().enumerate() {
            db.add_chapter(&format!("Ch {}", i + 1), &format!("{}{}", base, p), vol).unwrap();
        }
        (manga_scraper(&base), db)
    }

    /// Download every pending chapter with an attachment limit whose batch budget is `budget`
    async fn download_with_budget(scraper: &Arc<dyn SourceScraper>, db: &SourceDatabase, budget: u64) {
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };
        let limit = (budget + 1024 * 1024) as usize;
        ScraperClient::new().unwrap().download_all_chapters(scraper, db, 0, &images, limit).await.unwrap();
    }

    /// Five chapters downloaded with room for two per batch
    async fn five_chapters_two_per_batch() -> (Arc<dyn SourceScraper>, SourceDatabase) {
        let (scraper, db) = batch_setup(5, "Volume 1").await;
        download_with_budget(&scraper, &db, 2 * chapter_bytes() + 1).await;
        (scraper, db)
    }

    fn ch_in(n: u32, v: &str) -> (String, String) {
        (format!("Ch {}", n), v.to_string())
    }

    #[tokio::test]
    async fn chapters_fill_batches_by_budget_and_only_closing_flags_a_batch() {
        let (_scraper, db) = five_chapters_two_per_batch().await;

        assert_eq!(volume_of_each(&db), [
            ch_in(1, "Volume 1"), ch_in(2, "Volume 1"),
            ch_in(3, "Volume 2"), ch_in(4, "Volume 2"),
            ch_in(5, "Volume 3"),
        ]);
        assert_eq!(flagged_volumes(&db), ["Volume 1", "Volume 2"]);
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn an_oversized_first_chapter_is_a_batch_of_one() {
        let (scraper, db) = batch_setup(2, "Volume 1").await;

        download_with_budget(&scraper, &db, chapter_bytes() / 2).await;

        assert_eq!(volume_of_each(&db), [ch_in(1, "Volume 1"), ch_in(2, "Volume 2")]);
        assert_eq!(flagged_volumes(&db), ["Volume 1"]);
    }

    #[tokio::test]
    async fn a_source_named_volume_from_an_older_scrape_is_the_open_batch() {
        let (scraper, db) = batch_setup(2, "The JOJOLands").await;

        download_with_budget(&scraper, &db, chapter_bytes() + 1).await;

        assert_eq!(volume_of_each(&db), [ch_in(1, "The JOJOLands"), ch_in(2, "Volume 2")]);
        assert_eq!(flagged_volumes(&db), ["The JOJOLands"]);
    }

    #[test]
    fn a_pre_existing_volume_name_is_skipped_when_a_batch_closes() {
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let v1 = db.add_volume("Volume 1").unwrap();
        let open = db.add_volume("Volume 3").unwrap();
        db.add_chapter("Ch 1", "https://example.com/c1", v1).unwrap();
        db.add_chapter("Ch 2", "https://example.com/c2", open).unwrap();
        db.add_chapter("Ch 3", "https://example.com/c3", open).unwrap();
        let ids: Vec<isize> = { let mut c = db.get_empty_chapters().unwrap(); c.sort_by_key(|c| c.id); c.into_iter().map(|c| c.id).collect() };
        db.add_chapter_data(ids[0], "<a/>").unwrap();
        db.add_chapter_data(ids[1], "<b/>").unwrap();
        db.update_generated_volume(v1, false).unwrap();
        db.update_generated_volume(open, false).unwrap();
        let page = ChapterImage {
            chapter_id: ids[2], seq: 0,
            path: format!("images/{}-0.jpg", ids[2]),
            source_url: "https://example.com/p.jpg".into(),
            mime: "image/jpeg".into(), width: 1, height: 1, data: vec![0; 10],
        };
        let assembled = Assembled { html: "<c/>".into(), images: vec![page] };

        store_in_batch(&db, ids[2], &assembled, 5).unwrap();

        assert_eq!(volume_of_each(&db), [ch_in(1, "Volume 1"), ch_in(2, "Volume 3"), ch_in(3, "Volume 4")]);
        let stored = db.get_downloaded_chapters().unwrap().into_iter().find(|c| c.id == ids[2]).unwrap();
        assert_eq!(stored.volume_id, 3, "the new batch is a new volume");
        assert_eq!(flagged_volumes(&db), ["Volume 3"]);
        assert_eq!(db.count_volumes().unwrap(), 3);
    }

    #[tokio::test]
    async fn seed_chapter_parks_a_manga_chapter_in_the_open_batch_ignoring_volume() {
        let base = serve_fixtures(|_| vec![
            FixtureRoute { path: MANGA_CHAPTER_PATH, status: 200, content_type: "text/html", body: b"<html><head><title>Ch 1</title></head><body></body></html>".to_vec() },
        ]).await;
        let scraper = manga_scraper(&base);
        let url = format!("{}{}", base, MANGA_CHAPTER_PATH);
        let client = ScraperClient::new().unwrap();

        let empty = SourceDatabase::open_in_memory("t").unwrap();
        client.seed_chapter(&scraper, &empty, &url, None, Some("Volume 9")).await.unwrap();
        assert_eq!(volume_of_each(&empty), [ch_in(1, "Volume 1")]);
        assert_eq!(empty.count_volumes().unwrap(), 1);

        let two = SourceDatabase::open_in_memory("t").unwrap();
        two.add_volume("Volume 1").unwrap();
        two.add_volume("Volume 2").unwrap();
        client.seed_chapter(&scraper, &two, &url, None, Some("Volume 9")).await.unwrap();
        assert_eq!(volume_of_each(&two), [ch_in(1, "Volume 2")]);
        assert_eq!(two.count_volumes().unwrap(), 2);
    }

    #[tokio::test]
    async fn redownload_keeps_a_manga_chapter_in_its_batch_and_flags_nothing() {
        let (scraper, db) = five_chapters_two_per_batch().await;
        for c in db.get_downloaded_chapters().unwrap() {
            db.update_generated_chapter(c.id, false).unwrap();
            db.update_generated_volume(c.volume_id, false).unwrap();
        }
        let before = volume_of_each(&db);
        let images = ImagesConfig { request_delay: 0, ..ImagesConfig::default() };

        ScraperClient::new().unwrap().redownload_all(&scraper, &db, 0, &images).await.unwrap();

        assert_eq!(volume_of_each(&db), before);
        assert_eq!(db.count_volumes().unwrap(), 3);
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }

    #[test]
    fn extracts_wordpress_entry_title() {
        let html = r#"<html><head><title>10.69 IN – Test Serial</title></head>
            <body><h1 class="entry-title">10.69 IN</h1></body></html>"#;
        assert_eq!(extract_page_title(html), Some("10.69 IN".to_string()));
    }

    #[test]
    fn falls_back_to_title_tag_trimmed_at_separator() {
        let html =
            "<html><head><title>Chapter 5 | Example Site</title></head><body></body></html>";
        assert_eq!(extract_page_title(html), Some("Chapter 5".to_string()));
    }

    #[test]
    fn uses_whole_title_tag_when_no_separator() {
        let html = "<html><head><title>Chapter 5</title></head><body></body></html>";
        assert_eq!(extract_page_title(html), Some("Chapter 5".to_string()));
    }

    #[test]
    fn returns_none_without_any_title() {
        assert_eq!(
            extract_page_title("<html><body><p>hi</p></body></html>"),
            None
        );
    }

    #[test]
    fn derives_placeholder_title_from_url_slug() {
        assert_eq!(
            title_from_slug("https://example.com/2026/07/05/10-69-in/"),
            "10 69 in"
        );
    }
}
