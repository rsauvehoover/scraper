use thiserror::Error;

/// Errors that can occur during scraping operations
#[derive(Error, Debug)]
pub enum ScrapeError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Failed to parse HTML: {0}")]
    ParseError(String),

    #[error("Content not found: {0}")]
    ContentNotFound(String),

    #[error("Patreon-only content detected for chapter: {0}")]
    PatreonOnly(String),

    #[error("Database error: {0}")]
    DatabaseError(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Source not found: {0}")]
    SourceNotFound(String),

    #[error("Invalid configuration: {0}")]
    ConfigError(String),
}

/// Errors that can occur during EPUB generation
#[derive(Error, Debug)]
pub enum EpubError {
    #[error("Failed to generate EPUB: {0}")]
    GenerationError(String),

    #[error("Database error: {0}")]
    DatabaseError(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Image processing error: {0}")]
    ImageError(String),
}

/// Errors that can occur during database operations
#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    SqliteError(#[from] rusqlite::Error),

    #[error("Migration error: {0}")]
    MigrationError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Database not found: {0}")]
    NotFound(String),
}

/// Result type alias for scrape operations
pub type ScrapeResult<T> = Result<T, ScrapeError>;

/// Result type alias for EPUB operations
pub type EpubResult<T> = Result<T, EpubError>;

/// Result type alias for database operations
pub type DbResult<T> = Result<T, DbError>;
