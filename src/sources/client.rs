use reqwest::{header::HeaderValue, header::COOKIE, header::USER_AGENT, Client};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use soup::prelude::*;

use crate::config::{ImagesConfig, SourceKind};
use crate::db::{ChapterImage, SourceDatabase};
use crate::error::{ScrapeError, ScrapeResult};
use crate::images::{process_image, ProcessedImage};

use super::content_images::{assign_image_paths, drop_image};
use super::traits::SourceScraper;

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

    /// Fetch raw bytes from a URL, failing on non-success HTTP status
    pub async fn get_bytes(&self, url: &str) -> ScrapeResult<Vec<u8>> {
        let resp = self
            .client
            .get(url)
            .header(USER_AGENT, "reqwest")
            .send()
            .await
            .map_err(ScrapeError::Http)?;
        let resp = resp.error_for_status().map_err(ScrapeError::Http)?;
        let bytes = resp.bytes().await.map_err(ScrapeError::Http)?;
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

        for chapter in chapters {
            if ignored_volumes.contains(&chapter.volume_name) || ignored_chapters.contains(&chapter.title) {
                continue
            }
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

        let volume_name = match volume_override {
            Some(v) => v.to_string(),
            None => db
                .get_latest_volume()?
                .map(|v| v.name)
                .unwrap_or_else(|| "Main Story".to_string()),
        };

        let volume_id = db.add_volume(&volume_name)?;
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
                    .download_chapter(scraper, db, &chapter, auth_headers.as_ref(), images)
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
                    .store_chapter_content(chapter.id, &f.assembled.html, &f.assembled.images, false)
                    .map_err(ScrapeError::from),
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => stored += 1,
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
        db.store_chapter_content(chapter.id, &assembled.html, &assembled.images, true)?;

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
    use crate::test_support::{serve_fixtures, FixtureRoute};

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

        ScraperClient::new().unwrap().download_all_chapters(&scraper, &db, 0, &images).await.unwrap();

        let ch = db.get_downloaded_chapters().unwrap();
        assert_eq!(ch.len(), 1);
        let imgs = db.get_chapter_images(ch[0].id).unwrap();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].path, format!("images/{}-0.png", ch[0].id));
        let html = db.get_chapter_data(ch[0].id).unwrap();
        assert_eq!(html.matches("<img").count(), 1);
        assert!(html.contains(&imgs[0].path));
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

        assert!(db.get_chapter_data(id).unwrap().contains(&format!("images/{}-0.png", id)));
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
