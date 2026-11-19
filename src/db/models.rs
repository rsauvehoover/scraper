/// Represents a chapter in the database
#[derive(Debug, Clone)]
pub struct Chapter {
    pub id: isize,
    pub name: String,
    pub uri: String,
    pub volume_id: isize,
    #[allow(dead_code)]
    pub data_id: Option<isize>,
}

/// Represents a volume in the database
#[derive(Debug, Clone)]
pub struct Volume {
    pub id: isize,
    pub name: String,
}

/// Which EPUB regeneration flags `store_chapter_content` sets to whether the
/// HTML changed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegenerateFlags {
    /// Neither the chapter's nor its volume's flag is touched
    Untouched,
    /// Only the chapter's flag
    ChapterOnly,
    /// The chapter's and its volume's flags
    ChapterAndVolume,
}

/// An image downloaded for a chapter, stored in the source database
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterImage {
    pub chapter_id: isize,
    pub seq: u32,
    pub path: String,
    pub source_url: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}
