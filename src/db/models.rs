/// Represents a chapter in the database
#[derive(Debug, Clone)]
pub struct Chapter {
    pub id: usize,
    pub name: String,
    pub uri: String,
    pub volume_id: usize,
    #[allow(dead_code)]
    pub data_id: Option<usize>,
}

/// Represents a volume in the database
#[derive(Debug, Clone)]
pub struct Volume {
    pub id: usize,
    pub name: String,
}
