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
