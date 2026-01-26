use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MailConfig {
    pub name: String,
    pub address: String,
    pub password: String,
    pub smtp_hostname: String,
    pub smtp_port: u16,
    pub destinations: Vec<UserConfig>,
}
impl Default for MailConfig {
    fn default() -> Self {
        MailConfig {
            name: String::default(),
            address: String::default(),
            password: String::default(),
            smtp_hostname: String::from("smtp.gmail.com"),
            smtp_port: 587,
            destinations: Vec::<UserConfig>::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserConfig {
    pub name: String,
    pub email: String,
    pub strip_colour: bool,
    pub send_full_volumes: bool,
    pub send_individual_chapters: bool,
}
impl Default for UserConfig {
    fn default() -> Self {
        UserConfig {
            name: String::default(),
            email: String::default(),
            strip_colour: false,
            send_full_volumes: true,
            send_individual_chapters: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EpubGenConfig {
    pub volumes: bool,
    pub chapters: bool,
    pub strip_colour: bool,
}

impl Default for EpubGenConfig {
    fn default() -> Self {
        EpubGenConfig {
            volumes: true,
            chapters: true,
            strip_colour: false,
        }
    }
}

/// Selector type for HTML parsing
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SelectorType {
    Class,
    Id,
    Tag,
}

impl Default for SelectorType {
    fn default() -> Self {
        SelectorType::Class
    }
}

/// HTML selectors for parsing a source's TOC and chapters
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Selectors {
    /// CSS class/id/tag for volume containers
    pub volume_wrapper: String,
    /// Tag for volume title (inside volume wrapper)
    pub volume_title: String,
    /// CSS class/id/tag for chapter entries
    pub chapter_entry: String,
    /// Tag for chapter links (inside chapter entry)
    pub chapter_link: String,
    /// CSS id for main content container
    pub main_content: String,
    /// How to interpret the above selectors
    pub selector_type: SelectorType,
}

impl Default for Selectors {
    fn default() -> Self {
        Selectors {
            volume_wrapper: String::from("volume-wrapper"),
            volume_title: String::from("h2"),
            chapter_entry: String::from("chapter-entry"),
            chapter_link: String::from("a"),
            main_content: String::from("main-content"),
            selector_type: SelectorType::Class,
        }
    }
}

/// Authentication configuration for a source
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", tag = "Type")]
pub enum AuthConfig {
    /// No authentication required
    None,
    /// Patreon integration
    #[serde(rename_all = "PascalCase")]
    Patreon { patreon_name: String },
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig::None
    }
}

/// Metadata for a source (used in EPUB generation)
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SourceMetadata {
    pub author: String,
    pub description: String,
}

impl Default for SourceMetadata {
    fn default() -> Self {
        SourceMetadata {
            author: String::from("Unknown"),
            description: String::new(),
        }
    }
}

/// Configuration for a single source
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SourceConfig {
    /// Unique identifier for this source
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Whether this source is enabled
    pub enabled: bool,
    /// URL to the table of contents page
    pub toc_url: String,
    /// HTML selectors for parsing
    pub selectors: Selectors,
    /// Authentication configuration
    pub auth: AuthConfig,
    /// Metadata for EPUB generation
    pub metadata: SourceMetadata,
    /// Post-processors to apply (in order)
    pub post_processors: Vec<String>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        SourceConfig {
            id: String::from("unknown"),
            name: String::from("Unknown Source"),
            enabled: true,
            toc_url: String::new(),
            selectors: Selectors::default(),
            auth: AuthConfig::None,
            metadata: SourceMetadata::default(),
            post_processors: Vec::new(),
        }
    }
}

impl SourceConfig {
    /// Create a default Wandering Inn source configuration
    pub fn wandering_inn() -> Self {
        SourceConfig {
            id: String::from("wandering-inn"),
            name: String::from("The Wandering Inn"),
            enabled: true,
            toc_url: String::from("https://wanderinginn.com/table-of-contents/"),
            selectors: Selectors::default(),
            auth: AuthConfig::None,
            metadata: SourceMetadata {
                author: String::from("pirate aba"),
                description: String::from("The Wandering Inn"),
            },
            post_processors: vec![
                String::from("mrsha-write"),
                String::from("strip-links"),
            ],
        }
    }

    /// Create a Wandering Inn source with Patreon authentication
    pub fn wandering_inn_with_patreon(patreon_name: &str) -> Self {
        let mut config = Self::wandering_inn();
        config.auth = AuthConfig::Patreon {
            patreon_name: patreon_name.to_string(),
        };
        config
    }

    /// Create a Royal Road source configuration
    pub fn royal_road(fiction_id: &str, name: &str, author: &str, description: &str) -> Self {
        SourceConfig {
            id: format!("royal-road-{}", fiction_id),
            name: name.to_string(),
            enabled: true,
            toc_url: format!("https://www.royalroad.com/fiction/{}/{}", fiction_id, slug_name(name)),
            selectors: Selectors {
                volume_wrapper: String::from("volume-selector"),
                volume_title: String::from("h6"),
                chapter_entry: String::from("chapter-row"),
                chapter_link: String::from("a"),
                main_content: String::from("chapter-content"),
                selector_type: SelectorType::Class,
            },
            auth: AuthConfig::None,
            metadata: SourceMetadata {
                author: author.to_string(),
                description: description.to_string(),
            },
            post_processors: vec![String::from("strip-links")],
        }
    }
}

/// Convert a name to a URL slug
fn slug_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Config {
    pub mail: MailConfig,
    pub epub_gen: EpubGenConfig,
    /// Global request delay in milliseconds
    pub request_delay: u64,
    /// List of sources to scrape
    pub sources: Vec<SourceConfig>,

    // Legacy fields for backward compatibility
    #[serde(default)]
    pub toc_url: Option<String>,
    #[serde(default)]
    pub patreon_name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            request_delay: 1000,
            mail: MailConfig::default(),
            epub_gen: EpubGenConfig::default(),
            sources: vec![SourceConfig::wandering_inn()],
            toc_url: None,
            patreon_name: None,
        }
    }
}

impl Config {
    /// Get enabled sources only
    pub fn enabled_sources(&self) -> impl Iterator<Item = &SourceConfig> {
        self.sources.iter().filter(|s| s.enabled)
    }

    /// Find a source by ID
    pub fn find_source(&self, id: &str) -> Option<&SourceConfig> {
        self.sources.iter().find(|s| s.id == id)
    }
}

pub fn load_config() -> Config {
    if !std::path::Path::new("config.json").exists() {
        println!("No config.json found, using default values");
        println!("Request delay is 1000ms");
        return Config::default();
    }

    match std::fs::read_to_string("config.json") {
        Ok(str) => match serde_json::from_str::<Config>(&str) {
            Ok(mut config) => {
                println!("Loaded config");
                println!("Delay is {}ms", config.request_delay);
                println!(
                    "Sending from <{}> at <{}>",
                    config.mail.name, config.mail.address
                );
                for dest in &config.mail.destinations {
                    println!("Sending to <{}> at <{}>", dest.name, dest.email);
                }

                for dest in &config.mail.destinations {
                    if dest.strip_colour {
                        config.epub_gen.strip_colour = true;
                    }
                    if dest.send_full_volumes {
                        config.epub_gen.volumes = true;
                    }
                    if dest.send_individual_chapters {
                        config.epub_gen.chapters = true;
                    }
                }

                // Backward compatibility: migrate legacy config to multi-source format
                config = migrate_legacy_config(config);

                // Print enabled sources
                for source in config.enabled_sources() {
                    println!("Source enabled: {} ({})", source.name, source.id);
                }

                config
            }
            Err(e) => {
                panic!("{}", e);
            }
        },
        Err(e) => {
            panic!("{}", e);
        }
    }
}

/// Migrate legacy single-source config to multi-source format
fn migrate_legacy_config(mut config: Config) -> Config {
    // If Sources is empty but we have legacy toc_url, create a Wandering Inn source
    if config.sources.is_empty() {
        let source = if let Some(ref patreon_name) = config.patreon_name {
            if !patreon_name.is_empty() {
                let mut src = SourceConfig::wandering_inn_with_patreon(patreon_name);
                if let Some(ref toc_url) = config.toc_url {
                    src.toc_url = toc_url.clone();
                }
                src
            } else {
                let mut src = SourceConfig::wandering_inn();
                if let Some(ref toc_url) = config.toc_url {
                    src.toc_url = toc_url.clone();
                }
                src
            }
        } else {
            let mut src = SourceConfig::wandering_inn();
            if let Some(ref toc_url) = config.toc_url {
                src.toc_url = toc_url.clone();
            }
            src
        };

        config.sources.push(source);
        println!("Migrated legacy config to multi-source format");
    }

    config
}
