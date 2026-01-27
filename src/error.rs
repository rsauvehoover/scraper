use thiserror::Error;

/// Errors that can occur during scraping operations
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum ScrapeError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Failed to parse HTML: {0}")]
    Parse(String),

    #[error("Content not found: {0}")]
    ContentNotFound(String),

    #[error("Patreon-only content detected for chapter: {0}")]
    PatreonOnly(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Source not found: {0}")]
    SourceNotFound(String),

    #[error("Invalid configuration: {0}")]
    Config(String),
}

/// Errors that can occur during EPUB generation
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum EpubError {
    #[error("Failed to generate EPUB: {0}")]
    Generation(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Image processing error: {0}")]
    Image(String),
}

/// Errors that can occur during database operations
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database not found: {0}")]
    NotFound(String),
}

/// Result type alias for scrape operations
pub type ScrapeResult<T> = Result<T, ScrapeError>;

/// Result type alias for EPUB operations
#[allow(dead_code)]
pub type EpubResult<T> = Result<T, EpubError>;

/// Result type alias for database operations
pub type DbResult<T> = Result<T, DbError>;
