use rusqlite::{Connection, OptionalExtension, Params, Result};
use std::path::{Path, PathBuf};

use super::models::{Chapter, Volume};

/// Return both trailing-slash variants of a URI for slash-insensitive matching
fn uri_variants(uri: &str) -> (String, String) {
    let without = uri.trim_end_matches('/').to_string();
    let with = format!("{}/", without);
    (with, without)
}

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

    /// Open an in-memory database for tests
    #[cfg(test)]
    pub fn open_in_memory(source_id: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = SourceDatabase {
            conn,
            source_id: source_id.to_string(),
            db_path: PathBuf::new(),
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
    pub fn add_volume(&self, name: &str) -> Result<isize> {
        self.conn
            .prepare("INSERT OR IGNORE INTO volumes(name) VALUES(?1)")?
            .execute([name])?;
        self.conn
            .query_row("SELECT id FROM volumes WHERE name = ?1", [name], |row| {
                row.get(0)
            })
    }

    /// Get a volume name by ID
    pub fn get_volume_name(&self, volume_id: isize) -> Result<String> {
        self.conn.query_row(
            "SELECT name FROM volumes WHERE id = ?1",
            [volume_id],
            |row| row.get(0),
        )
    }

    /// Get the most recently added volume (highest id)
    pub fn get_latest_volume(&self) -> Result<Option<Volume>> {
        self.conn
            .query_row(
                "SELECT id, name FROM volumes ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok(Volume {
                        id: row.get(0)?,
                        name: row.get(1)?,
                    })
                },
            )
            .optional()
    }

    /// Add a chapter to the database
    pub fn add_chapter(&self, name: &str, uri: &str, volume_id: isize) -> Result<()> {
        self.conn
            .prepare("INSERT OR IGNORE INTO chapters(name, uri, volumeid) VALUES(?1, ?2, ?3)")?
            .execute((name, uri, volume_id))?;
        Ok(())
    }

    /// Remove a chapter from the database
    pub fn remove_chapter(&self, chapter_id: isize) -> Result<()> {
        self.conn
            .prepare("DELETE FROM chapters WHERE id = ?1")?
            .execute([chapter_id])?;
        Ok(())
    }

    /// Check if a chapter exists by URI (trailing-slash insensitive)
    pub fn chapter_exists_by_uri(&self, uri: &str) -> Result<bool> {
        let (with_slash, without_slash) = uri_variants(uri);
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM chapters WHERE uri IN (?1, ?2)",
            [with_slash.as_str(), without_slash.as_str()],
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

    /// Insert a chapter from the TOC, or update the existing row with the
    /// same URI (trailing-slash insensitive) in place.
    ///
    /// Preserves data_id and never touches regenerate_epub, so a
    /// title/volume correction for a manually pulled or discovered chapter
    /// does not trigger a re-download or re-send.
    pub fn upsert_chapter_from_toc(
        &self,
        name: &str,
        uri: &str,
        volume_id: isize,
    ) -> Result<()> {
        let (with_slash, without_slash) = uri_variants(uri);
        let existing: Option<(isize, String, Option<isize>)> = self
            .conn
            .query_row(
                "SELECT id, name, volumeid FROM chapters WHERE uri IN (?1, ?2) ORDER BY id LIMIT 1",
                [with_slash.as_str(), without_slash.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        match existing {
            Some((id, existing_name, existing_volume)) => {
                if existing_name != name || existing_volume != Some(volume_id) {
                    // OR IGNORE: pre-existing duplicate rows could collide
                    // with UNIQUE(name, uri, volumeid)
                    self.conn
                        .prepare(
                            "UPDATE OR IGNORE chapters SET name = ?1, volumeid = ?2 WHERE id = ?3",
                        )?
                        .execute((name, volume_id, id))?;
                }
                Ok(())
            }
            None => self.add_chapter(name, uri, volume_id),
        }
    }

    /// Add or update chapter data
    pub fn add_chapter_data(&self, chapter_id: isize, data: &str) -> Result<()> {
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
        let data_id: isize =
            self.conn
                .query_row("SELECT id FROM raw_data WHERE data = ?1", [data], |row| {
                    row.get(0)
                })?;

        self.conn
            .prepare("UPDATE chapters SET data_id = ?1, regenerate_epub = ?2 WHERE id = ?3")?
            .execute([data_id, regenerate as isize, chapter_id])?;

        let volume_id: isize = self.conn.query_row(
            "SELECT volumeid FROM chapters WHERE id = ?1",
            [chapter_id],
            |row| row.get(0),
        )?;

        self.conn
            .prepare("UPDATE volumes SET regenerate_epub = ?1 WHERE id = ?2")?
            .execute([regenerate as isize, volume_id])?;

        Ok(())
    }

    /// Get chapter data by chapter ID
    pub fn get_chapter_data(&self, chapter_id: isize) -> Result<String> {
        self.conn.query_row(
            "SELECT data FROM raw_data WHERE chapter_id = ?1",
            [chapter_id],
            |row| row.get(0),
        )
    }

    /// Get all chapters for a volume
    pub fn get_chapters_by_volume(&self, volume_id: isize) -> Result<Vec<Chapter>> {
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
        self.volume_query("SELECT id, name FROM volumes WHERE regenerate_epub = 1", [])
    }

    /// Update volume regeneration flag
    pub fn update_generated_volume(&self, id: isize, regenerate: bool) -> Result<()> {
        self.conn
            .prepare("UPDATE volumes SET regenerate_epub = ?1 WHERE id = ?2")?
            .execute([regenerate as isize, id])?;
        Ok(())
    }

    /// Update chapter regeneration flag
    pub fn update_generated_chapter(&self, id: isize, regenerate: bool) -> Result<()> {
        self.conn
            .prepare("UPDATE chapters SET regenerate_epub = ?1 WHERE id = ?2")?
            .execute([regenerate as isize, id])?;
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
                    data_id: row.get::<_, Option<isize>>(4)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> SourceDatabase {
        SourceDatabase::open_in_memory("test-source").unwrap()
    }

    #[test]
    fn chapter_exists_by_uri_ignores_trailing_slash() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("Chapter 1", "https://example.com/chapter-1/", vol)
            .unwrap();

        assert!(db
            .chapter_exists_by_uri("https://example.com/chapter-1/")
            .unwrap());
        assert!(db
            .chapter_exists_by_uri("https://example.com/chapter-1")
            .unwrap());
        assert!(!db
            .chapter_exists_by_uri("https://example.com/chapter-2")
            .unwrap());
    }

    #[test]
    fn get_latest_volume_returns_highest_id() {
        let db = test_db();
        assert!(db.get_latest_volume().unwrap().is_none());

        db.add_volume("Volume 1").unwrap();
        db.add_volume("Volume 2").unwrap();

        let latest = db.get_latest_volume().unwrap().unwrap();
        assert_eq!(latest.name, "Volume 2");
    }

    fn all_chapters(db: &SourceDatabase) -> Vec<(String, String, Option<isize>, Option<isize>, isize)> {
        db.connection()
            .prepare("SELECT name, uri, volumeid, data_id, regenerate_epub FROM chapters ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn upsert_inserts_when_uri_absent() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();

        db.upsert_chapter_from_toc("Chapter 1", "https://example.com/chapter-1/", vol)
            .unwrap();

        let rows = all_chapters(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "Chapter 1");
        assert_eq!(rows[0].2, Some(vol));
    }

    #[test]
    fn upsert_updates_existing_row_without_resend() {
        let db = test_db();
        let vol1 = db.add_volume("Volume 1").unwrap();
        // Simulate a manual pull: placeholder-ish title, trailing slash,
        // guessed volume, content already downloaded and sent.
        db.add_chapter("parsed title", "https://example.com/chapter-1/", vol1)
            .unwrap();
        let id: isize = db
            .connection()
            .query_row(
                "SELECT id FROM chapters WHERE uri = ?1",
                ["https://example.com/chapter-1/"],
                |row| row.get(0),
            )
            .unwrap();
        db.add_chapter_data(id, "<html>content</html>").unwrap();
        // Simulate the epub having been generated and sent already
        db.update_generated_chapter(id, false).unwrap();
        db.update_generated_volume(vol1, false).unwrap();

        // TOC catches up: real title, no trailing slash, different volume
        let vol2 = db.add_volume("Volume 2").unwrap();
        db.upsert_chapter_from_toc("Chapter 1", "https://example.com/chapter-1", vol2)
            .unwrap();

        let rows = all_chapters(&db);
        assert_eq!(rows.len(), 1, "TOC sync must not create a duplicate row");
        let (name, _uri, volumeid, data_id, regenerate) = rows[0].clone();
        assert_eq!(name, "Chapter 1");
        assert_eq!(volumeid, Some(vol2));
        assert!(data_id.is_some(), "downloaded content must be preserved");
        assert_eq!(regenerate, 0, "correction must not trigger a re-send");
    }

    #[test]
    fn upsert_is_noop_when_row_matches_toc() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.upsert_chapter_from_toc("Chapter 1", "https://example.com/chapter-1/", vol)
            .unwrap();
        db.upsert_chapter_from_toc("Chapter 1", "https://example.com/chapter-1/", vol)
            .unwrap();

        let rows = all_chapters(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].4, 0);
    }

    #[test]
    fn upsert_ignores_update_that_would_collide_with_unique_constraint() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        // Legacy duplicate rows: same uri and volume, distinct names, which
        // the UNIQUE(name, uri, volumeid) constraint permits.
        db.add_chapter("Chapter A", "https://example.com/chapter-1/", vol)
            .unwrap();
        db.add_chapter("Chapter B", "https://example.com/chapter-1/", vol)
            .unwrap();

        // The lookup picks the lower-id row ("Chapter A"); renaming it to
        // "Chapter B" would collide with the second row's UNIQUE tuple, so
        // OR IGNORE must skip the update entirely.
        db.upsert_chapter_from_toc("Chapter B", "https://example.com/chapter-1/", vol)
            .unwrap();

        let rows = all_chapters(&db);
        assert_eq!(rows.len(), 2, "no row should be inserted or removed");
        assert_eq!(rows[0].0, "Chapter A", "collision must leave the row untouched");
        assert_eq!(rows[1].0, "Chapter B");
        for row in &rows {
            assert!(row.3.is_none(), "no data should be attached");
            assert_eq!(row.4, 0, "regenerate flag must not be set");
        }
    }
}
