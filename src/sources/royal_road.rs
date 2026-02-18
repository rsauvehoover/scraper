use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use soup::prelude::*;
use std::collections::HashMap;

use crate::config::SourceConfig;
use crate::error::{ScrapeError, ScrapeResult};

use super::traits::{ChapterContent, ScrapedChapter, SourceScraper};

/// Volume data from Royal Road's JavaScript
#[derive(Debug, Deserialize)]
struct RoyalRoadVolume {
    id: i64,
    title: String,
}

/// Chapter data from Royal Road's JavaScript
#[derive(Debug, Deserialize)]
struct RoyalRoadChapter {
    #[serde(rename = "volumeId")]
    volume_id: Option<i64>,
    title: String,
    url: String,
}

/// Scraper for Royal Road fiction
pub struct RoyalRoadScraper {
    config: SourceConfig,
}

impl RoyalRoadScraper {
    pub fn new(config: SourceConfig) -> Self {
        RoyalRoadScraper { config }
    }

    /// Extract the base URL from a Royal Road fiction URL
    fn base_url(&self) -> &str {
        "https://www.royalroad.com"
    }

    /// Normalize a chapter URL (handle relative URLs)
    fn normalize_url(&self, url: &str) -> String {
        if url.starts_with("http") {
            url.to_string()
        } else if url.starts_with('/') {
            format!("{}{}", self.base_url(), url)
        } else {
            url.to_string()
        }
    }

    /// Extract volumes array from Royal Road's JavaScript
    fn extract_volumes(html: &str) -> Vec<RoyalRoadVolume> {
        let re = Regex::new(r"window\.volumes\s*=\s*(\[[\s\S]*?\]);").unwrap();
        if let Some(caps) = re.captures(html) {
            if let Some(json_str) = caps.get(1) {
                if let Ok(volumes) = serde_json::from_str::<Vec<RoyalRoadVolume>>(json_str.as_str())
                {
                    return volumes;
                }
            }
        }
        Vec::new()
    }

    /// Extract chapters array from Royal Road's JavaScript
    fn extract_chapters(html: &str) -> Vec<RoyalRoadChapter> {
        let re = Regex::new(r"window\.chapters\s*=\s*(\[[\s\S]*?\]);").unwrap();
        if let Some(caps) = re.captures(html) {
            if let Some(json_str) = caps.get(1) {
                if let Ok(chapters) =
                    serde_json::from_str::<Vec<RoyalRoadChapter>>(json_str.as_str())
                {
                    return chapters;
                }
            }
        }
        Vec::new()
    }
}

#[async_trait]
impl SourceScraper for RoyalRoadScraper {
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
        let mut chapters = Vec::new();

        // Try to extract chapter and volume data from JavaScript objects first
        let js_volumes = Self::extract_volumes(html);
        let js_chapters = Self::extract_chapters(html);

        if !js_chapters.is_empty() {
            // Build volume lookup map
            let volume_map: HashMap<i64, String> =
                js_volumes.into_iter().map(|v| (v.id, v.title)).collect();

            // Determine default volume name for chapters without a volumeId
            let default_volume = if volume_map.is_empty() {
                "Main Story".to_string()
            } else {
                "Uncategorized".to_string()
            };

            for chapter in js_chapters {
                let volume_name = chapter
                    .volume_id
                    .and_then(|id| volume_map.get(&id).cloned())
                    .unwrap_or_else(|| default_volume.clone());

                let uri = self.normalize_url(&chapter.url);

                if !chapter.title.is_empty() {
                    chapters.push(ScrapedChapter {
                        title: chapter.title,
                        uri,
                        volume_name,
                    });
                }
            }

            println!(
                "({}) Found {} chapters in {} volumes from JavaScript data",
                self.source_id(),
                chapters.len(),
                volume_map.len().max(1)
            );
        } else {
            // Fallback to HTML parsing if JavaScript extraction failed
            let soup = Soup::new(html);
            let chapter_rows = soup.class("chapter-row").find_all();
            let default_volume = "Main Story".to_string();

            for row in chapter_rows {
                if let Some(link) = row.tag("a").find() {
                    if let Some(href) = link.get("href") {
                        let title = link.text().trim().to_string();
                        let uri = self.normalize_url(&href);

                        if !title.is_empty() {
                            chapters.push(ScrapedChapter {
                                title,
                                uri,
                                volume_name: default_volume.clone(),
                            });
                        }
                    }
                }
            }

            // If we found no chapters via chapter-row, try tbody tr as fallback
            if chapters.is_empty() {
                if let Some(table) = soup.attr("id", "chapters").find() {
                    for row in table.tag("tr").find_all() {
                        if let Some(link) = row.tag("a").find() {
                            if let Some(href) = link.get("href") {
                                let title = link.text().trim().to_string();
                                let uri = self.normalize_url(&href);

                                if !title.is_empty() && href.contains("/chapter/") {
                                    chapters.push(ScrapedChapter {
                                        title,
                                        uri,
                                        volume_name: default_volume.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            println!(
                "({}) Found {} chapters in TOC via HTML parsing (may discover more during download)",
                self.source_id(),
                chapters.len()
            );
        }

        Ok(chapters)
    }

    async fn parse_chapter(&self, html: &str, title: &str) -> ScrapeResult<ChapterContent> {
        // Fix HTML entity escaping issues
        let escape_re = Regex::new(r"(?:&)((?:lt|gt|nbsp);)").unwrap();
        let html_string = escape_re
            .replace_all(html, |captures: &regex::Captures| {
                format!("&amp;{}", &captures[1])
            })
            .to_string();

        let soup = Soup::new(&html_string);

        // Royal Road chapter content is in div with both "chapter-inner" and "chapter-content" classes
        let content = soup.class("chapter-content").find().ok_or_else(|| {
            ScrapeError::ContentNotFound("Chapter content element not found".to_string())
        })?;

        // Extract style tags that hide injected elements (Royal Road uses display:none to hide them)
        let hidden_styles: String = soup
            .tag("style")
            .find_all()
            .filter(|style| {
                let text = style.text();
                text.contains("display:none") || text.contains("display: none")
            })
            .map(|style| style.display())
            .collect::<Vec<_>>()
            .join("\n");

        // Extract next chapter link from <link rel="next"> in head
        let next_chapter_url = soup
            .tag("link")
            .find_all()
            .find(|link| link.get("rel").as_deref() == Some("next"))
            .and_then(|link| link.get("href"))
            .map(|url| self.normalize_url(&url));

        // Try to get next chapter title from the navigation button
        let next_chapter_title = if next_chapter_url.is_some() {
            // The next chapter button contains "Next Chapter" text, not the actual title
            // We'll extract the title when we download the chapter
            None
        } else {
            None
        };

        // Get chapter title from h1 if available, otherwise use provided title
        let chapter_title = soup
            .tag("h1")
            .find()
            .map(|h| h.text().trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| title.to_string());

        // Build XHTML document, including hidden style tags to suppress injected elements
        let header = format!(
            r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
<meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />
<meta name="author" content="{}"/>
<meta name="description" content="{}"/>
<meta name="classification" content="Fantasy" />
<title>{}</title>
<link rel="stylesheet" href="style.css" type = "text/css" />
{}
</head>
<body>"#,
            self.config.metadata.author,
            self.config.metadata.description,
            self.config.name,
            hidden_styles
        );

        let chapter_heading = format!("<h1>{}</h1>", chapter_title);
        let body = content.display();
        let footer = "</body></html>";

        let full_html = format!("{}\n{}\n{}\n{}\n", header, chapter_heading, body, footer);

        Ok(ChapterContent {
            title: chapter_title,
            html: full_html,
            is_patreon_only: false, // Royal Road doesn't have Patreon-only chapters in the same way
            next_chapter_url,
            next_chapter_title,
        })
    }

    fn build_auth_headers(&self) -> Option<Vec<(String, String)>> {
        // Royal Road doesn't require authentication for public chapters
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
}
