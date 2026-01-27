use async_trait::async_trait;
use regex::Regex;
use soup::prelude::*;

use crate::config::{AuthConfig, SelectorType, SourceConfig};
use crate::error::{ScrapeError, ScrapeResult};

use super::traits::{ChapterContent, ScrapedChapter, SourceScraper};

/// Generic scraper that uses configurable selectors
pub struct GenericScraper {
    config: SourceConfig,
}

impl GenericScraper {
    pub fn new(config: SourceConfig) -> Self {
        GenericScraper { config }
    }
}

#[async_trait]
impl SourceScraper for GenericScraper {
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
        let soup = Soup::new(html);
        let mut chapters = Vec::new();

        let selectors = &self.config.selectors;

        // Find volumes - always use class selector for volume wrapper (most common pattern)
        let volumes: Vec<_> = match selectors.selector_type {
            SelectorType::Class => soup
                .class(selectors.volume_wrapper.as_str())
                .find_all()
                .collect(),
            SelectorType::Id => soup
                .attr("id", selectors.volume_wrapper.as_str())
                .find()
                .into_iter()
                .collect(),
            SelectorType::Tag => soup
                .tag(selectors.volume_wrapper.as_str())
                .find_all()
                .collect(),
        };

        for volume in volumes {
            // Get volume title
            let volume_title = volume
                .tag(selectors.volume_title.as_str())
                .find()
                .map(|t| t.text())
                .unwrap_or_else(|| "Unknown Volume".to_string());

            // Find chapters within this volume
            let chapter_entries: Vec<_> = match selectors.selector_type {
                SelectorType::Class => volume
                    .class(selectors.chapter_entry.as_str())
                    .find_all()
                    .collect(),
                SelectorType::Id => volume
                    .attr("id", selectors.chapter_entry.as_str())
                    .find()
                    .into_iter()
                    .collect(),
                SelectorType::Tag => volume
                    .tag(selectors.chapter_entry.as_str())
                    .find_all()
                    .collect(),
            };

            for chapter in chapter_entries {
                // Get chapter link
                if let Some(link) = chapter.tag(selectors.chapter_link.as_str()).find() {
                    if let Some(uri) = link.get("href") {
                        let title = link.text().trim().to_string();
                        chapters.push(ScrapedChapter {
                            title,
                            uri,
                            volume_name: volume_title.clone(),
                        });
                    }
                }
            }
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

        // Find main content by ID
        let content = soup
            .attr("id", self.config.selectors.main_content.as_str())
            .find()
            .ok_or_else(|| {
                ScrapeError::ContentNotFound(format!(
                    "Main content element '{}' not found",
                    self.config.selectors.main_content
                ))
            })?;

        // Check for Patreon-only content
        let page_title = soup
            .tag("title")
            .find()
            .map(|t| t.text())
            .unwrap_or_default();
        let patron_re = Regex::new(r"(?i)Patron Early Access").unwrap();
        let is_patreon_only = patron_re.is_match(&page_title);

        // Build XHTML document
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
</head>
<body>"#,
            self.config.metadata.author, self.config.metadata.description, self.config.name
        );

        let chapter_title = format!("<h1>{}</h1>", title);
        let body = content.display();
        let footer = "</body></html>";

        let full_html = format!("{}\n{}\n{}\n{}\n", header, chapter_title, body, footer);

        Ok(ChapterContent {
            title: title.to_string(),
            html: full_html,
            is_patreon_only,
            next_chapter_url: None,
            next_chapter_title: None,
        })
    }

    fn build_auth_headers(&self) -> Option<Vec<(String, String)>> {
        match &self.config.auth {
            AuthConfig::None => None,
            AuthConfig::Patreon { patreon_name } => {
                if patreon_name.is_empty() {
                    return None;
                }

                let epoch_stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();

                let cookie = format!(
                    "patreon_verified=1; patreon_verified_time={}; patreon_patron_status=active_patron; patreon_user_name={};",
                    epoch_stamp, patreon_name
                );

                Some(vec![("Cookie".to_string(), cookie)])
            }
        }
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
