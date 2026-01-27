use rusqlite::{Connection, OptionalExtension, Params, Result};
use std::path::{Path, PathBuf};

use super::models::{Chapter, Volume};

/// Database connection for a specific source
pub struct SourceDatabase {
    conn: Connection,
    #[allow(dead_code)]
    source_id: String,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl SourceDatabase {
    /// Open or create a database for a source
    pub fn open(source_id: &str) -> Result<Self> {
        let db_dir = Path::new("db");
        std::fs::create_dir_all(db_dir).unwrap();

        let db_path = db_dir.join(format!("{}.db", source_id));
        let conn = Connection::open(&db_path)?;

        let db = SourceDatabase {
            conn,
            source_id: source_id.to_string(),
            db_path,
        };

        db.initialize_schema()?;
        Ok(db)
    }

    /// Get the source ID for this database
    #[allow(dead_code)]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Get the database file path
    #[allow(dead_code)]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Get a reference to the underlying connection
    #[allow(dead_code)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Initialize the database schema
    fn initialize_schema(&self) -> Result<()> {
        // Source metadata table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS source_metadata(
                key TEXT PRIMARY KEY,
                value TEXT
            )",
            (),
        )?;

        // Store source ID in metadata
        self.conn.execute(
            "INSERT OR REPLACE INTO source_metadata(key, value) VALUES ('source_id', ?1)",
            [&self.source_id],
        )?;

        // Volumes table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS volumes(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                regenerate_epub INTEGER DEFAULT 0 CHECK(regenerate_epub IN (0, 1)),
                UNIQUE(name)
            )",
            (),
        )?;

        // Chapters table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS chapters(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                uri TEXT NOT NULL,
                volumeid INTEGER,
                data_id INTEGER,
                regenerate_epub INTEGER DEFAULT 0 CHECK(regenerate_epub IN (0, 1)),
                FOREIGN KEY(data_id) REFERENCES raw_data(id),
                FOREIGN KEY(volumeid) REFERENCES volumes(id),
                UNIQUE(name, uri, volumeid)
            )",
            (),
        )?;

        // Raw data table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS raw_data(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chapter_id INTEGER,
                data TEXT,
                FOREIGN KEY(chapter_id) REFERENCES chapters(id),
                UNIQUE(chapter_id)
            )",
            (),
        )?;

        Ok(())
    }

    /// Add a volume to the database
    pub fn add_volume(&self, name: &str) -> Result<usize> {
        self.conn
            .prepare("INSERT OR IGNORE INTO volumes(name) VALUES(?1)")?
            .execute([name])?;
        self.conn.query_row(
            "SELECT id FROM volumes WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
    }

    /// Get a volume name by ID
    pub fn get_volume_name(&self, volume_id: usize) -> Result<String> {
        self.conn.query_row(
            "SELECT name FROM volumes WHERE id = ?1",
            [volume_id],
            |row| row.get(0),
        )
    }

    /// Add a chapter to the database
    pub fn add_chapter(&self, name: &str, uri: &str, volume_id: usize) -> Result<()> {
        self.conn
            .prepare("INSERT OR IGNORE INTO chapters(name, uri, volumeid) VALUES(?1, ?2, ?3)")?
            .execute((name, uri, volume_id))?;
        Ok(())
    }

    /// Remove a chapter from the database
    pub fn remove_chapter(&self, chapter_id: usize) -> Result<()> {
        self.conn
            .prepare("DELETE FROM chapters WHERE id = ?1")?
            .execute([chapter_id])?;
        Ok(())
    }

    /// Check if a chapter exists by URI
    pub fn chapter_exists_by_uri(&self, uri: &str) -> Result<bool> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM chapters WHERE uri = ?1",
            [uri],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Add a discovered chapter (from next chapter links)
    /// Returns true if the chapter was newly added, false if it already existed
    pub fn add_discovered_chapter(&self, name: &str, uri: &str, volume_name: &str) -> Result<bool> {
        if self.chapter_exists_by_uri(uri)? {
            return Ok(false);
        }

        let volume_id = self.add_volume(volume_name)?;
        self.add_chapter(name, uri, volume_id)?;
        Ok(true)
    }

    /// Add or update chapter data
    pub fn add_chapter_data(&self, chapter_id: usize, data: &str) -> Result<()> {
        let existing_data: String = self
            .conn
            .query_row(
                "SELECT data FROM raw_data WHERE chapter_id = ?1",
                [chapter_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();

        self.conn
            .prepare("INSERT OR REPLACE INTO raw_data(data, chapter_id) VALUES(?1, ?2)")?
            .execute((data, chapter_id))?;

        let regenerate = !existing_data.eq(data);
        let data_id: usize = self.conn.query_row(
            "SELECT id FROM raw_data WHERE data = ?1",
            [data],
            |row| row.get(0),
        )?;

        self.conn
            .prepare("UPDATE chapters SET data_id = ?1, regenerate_epub = ?2 WHERE id = ?3")?
            .execute([data_id, regenerate as usize, chapter_id])?;

        let volume_id: usize = self.conn.query_row(
            "SELECT volumeid FROM chapters WHERE id = ?1",
            [chapter_id],
            |row| row.get(0),
        )?;

        self.conn
            .prepare("UPDATE volumes SET regenerate_epub = ?1 WHERE id = ?2")?
            .execute([regenerate as usize, volume_id])?;

        Ok(())
    }

    /// Get chapter data by chapter ID
    pub fn get_chapter_data(&self, chapter_id: usize) -> Result<String> {
        self.conn.query_row(
            "SELECT data FROM raw_data WHERE chapter_id = ?1",
            [chapter_id],
            |row| row.get(0),
        )
    }

    /// Get all chapters for a volume
    pub fn get_chapters_by_volume(&self, volume_id: usize) -> Result<Vec<Chapter>> {
        self.chapter_query(
            "SELECT id, name, uri, volumeid, data_id FROM chapters WHERE volumeid = ?1",
            [volume_id],
        )
    }

    /// Get chapters without data (need downloading)
    pub fn get_empty_chapters(&self) -> Result<Vec<Chapter>> {
        self.chapter_query(
            "SELECT id, name, uri, volumeid, data_id FROM chapters WHERE data_id IS NULL",
            [],
        )
    }

    /// Get chapters needing EPUB regeneration
    pub fn get_chapters_to_regenerate(&self) -> Result<Vec<Chapter>> {
        self.chapter_query(
            "SELECT id, name, uri, volumeid, data_id FROM chapters WHERE regenerate_epub = 1",
            [],
        )
    }

    /// Get volumes needing EPUB regeneration
    pub fn get_volumes_to_regenerate(&self) -> Result<Vec<Volume>> {
        self.volume_query(
            "SELECT id, name FROM volumes WHERE regenerate_epub = 1",
            [],
        )
    }

    /// Update volume regeneration flag
    pub fn update_generated_volume(&self, id: usize, regenerate: bool) -> Result<()> {
        self.conn
            .prepare("UPDATE volumes SET regenerate_epub = ?1 WHERE id = ?2")?
            .execute([regenerate as usize, id])?;
        Ok(())
    }

    /// Update chapter regeneration flag
    pub fn update_generated_chapter(&self, id: usize, regenerate: bool) -> Result<()> {
        self.conn
            .prepare("UPDATE chapters SET regenerate_epub = ?1 WHERE id = ?2")?
            .execute([regenerate as usize, id])?;
        Ok(())
    }

    /// Helper function for chapter queries
    fn chapter_query<P>(&self, sql: &str, params: P) -> Result<Vec<Chapter>>
    where
        P: Params,
    {
        self.conn
            .prepare(sql)?
            .query_map(params, |row| {
                Ok(Chapter {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    uri: row.get(2)?,
                    volume_id: row.get(3)?,
                    data_id: row.get::<_, Option<usize>>(4)?,
                })
            })?
            .collect()
    }

    /// Helper function for volume queries
    fn volume_query<P>(&self, sql: &str, params: P) -> Result<Vec<Volume>>
    where
        P: Params,
    {
        self.conn
            .prepare(sql)?
            .query_map(params, |row| {
                Ok(Volume {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })?
            .collect()
    }
}
