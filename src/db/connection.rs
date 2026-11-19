use rusqlite::{Connection, OptionalExtension, Params, Result};
use std::path::{Path, PathBuf};

use super::models::{Chapter, ChapterImage, RegenerateFlags, Volume};

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

    /// Open an existing source database for reading only.
    ///
    /// Deliberately NOT `SQLITE_OPEN_READ_ONLY`: a WAL reader must be able to
    /// write the `-shm` index, so a read-only handle fails against exactly the
    /// databases this project uses. `PRAGMA query_only` enforces the guarantee
    /// at the SQLite level instead. The process still needs filesystem write
    /// permission on `db/` even though it never writes through SQLite.
    ///
    /// `SQLITE_OPEN_CREATE` is left out on purpose. `Connection::open` sets it,
    /// and it creates a zero-byte file for a database that does not exist —
    /// which would make this process write to `db/` after all, contradicting
    /// what the README promises, and would turn "this source has never been
    /// scraped" into a silently empty database instead of an error. Without
    /// the flag, a missing file is `SqliteFailure(14, "unable to open database
    /// file")` and the caller decides what to do about it.
    ///
    /// Does not call `initialize_schema`: schema creation is a write, and this
    /// handle cannot write. A database that opens is therefore not necessarily
    /// one the scraper has ever populated — see `has_scraper_schema`.
    ///
    /// Deliberately `pub(crate)`, not `pub`. `source_id` is interpolated into
    /// a path with no validation here, so this function does nothing to stop a
    /// traversal on its own — what stops it is that both callers
    /// (`SourceRegistry::from_config`, and `web::download`'s blocking tasks,
    /// which run only after `SourceRegistry::get` has matched the id against
    /// the configured list) pass a string that has already been compared
    /// against the configured IDs. That is a convention this signature cannot
    /// enforce; keeping the constructor crate-private keeps the set of places
    /// that have to honour it small enough to check by reading them.
    pub(crate) fn open_query_only(source_id: &str) -> Result<Self> {
        use rusqlite::OpenFlags;

        let db_path = Self::path_for(source_id);
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(SourceDatabase {
            conn,
            source_id: source_id.to_string(),
            db_path,
        })
    }

    /// Where `open_query_only` would look for `source_id`'s database, without
    /// opening it.
    ///
    /// Exists so a caller that just got an open failure can tell "the file
    /// isn't there yet" (a source added to config but never scraped) apart
    /// from any other reason the open could fail, by checking this path
    /// itself — without duplicating the `db/{id}.db` convention at the call
    /// site.
    pub(crate) fn path_for(source_id: &str) -> PathBuf {
        Path::new("db").join(format!("{}.db", source_id))
    }

    /// Whether this database carries the tables `initialize_schema` creates.
    ///
    /// A query-only handle cannot create them, so a source that has been added
    /// to `config.json` but never scraped opens fine and then fails every
    /// query with "no such table". Callers use this to tell that apart from a
    /// database that is merely empty.
    pub(crate) fn has_scraper_schema(&self) -> Result<bool> {
        let found: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN ('volumes', 'chapters', 'raw_data')",
            [],
            |row| row.get(0),
        )?;
        Ok(found == 3)
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
        // WAL lets the web process read while a scheduled scraper run writes:
        // readers never block the writer and the writer never blocks readers. The
        // setting is persistent in the file header, so the scraper applies it once
        // and the web process inherits it.
        //
        // `PRAGMA journal_mode=WAL` returns the resulting mode as a row, so it
        // cannot be issued through `pragma_update` (which expects no rows and
        // errors with `ExecuteReturnedResults`). An in-memory database cannot
        // use WAL and reports "memory" instead; a file-backed database reports
        // "wal". Both are success — anything else is a genuine failure and
        // propagates.
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if mode != "wal" && mode != "memory" {
            // Not fatal: the database still works in any journal mode, just
            // with readers and the writer blocking each other. But this must
            // be visible, because every claim about concurrent access rests
            // on WAL actually being in effect -- and a check that only runs
            // in debug builds would vanish from the release build that
            // actually runs.
            eprintln!(
                "warning: requested WAL journal mode for {} but SQLite reports '{}'; \
                 readers and the writer will block each other",
                self.source_id, mode
            );
        }

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

        // Images embedded in chapters
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS images(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chapter_id INTEGER NOT NULL REFERENCES chapters(id),
                seq INTEGER NOT NULL,
                path TEXT NOT NULL,
                source_url TEXT NOT NULL,
                mime TEXT NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                data BLOB NOT NULL,
                UNIQUE(chapter_id, seq),
                UNIQUE(path)
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

    /// Every volume, oldest first
    pub fn get_all_volumes(&self) -> Result<Vec<Volume>> {
        self.conn
            .prepare("SELECT id, name FROM volumes ORDER BY id")?
            .query_map([], |row| Ok(Volume { id: row.get(0)?, name: row.get(1)? }))?
            .collect()
    }

    /// Move every chapter into one volume named `name` and delete the rest.
    /// The migration from the manga batch design: a volume already named
    /// `name` is the target, else the oldest volume is renamed, else one is
    /// created. The target's regenerate flag is cleared and chapter flags are
    /// left as they are, so collapsing never queues a send. Returns how many
    /// chapters moved; 0 once the source has its single volume. A chapter
    /// present in two volumes violates `UNIQUE(name, uri, volumeid)` and
    /// rolls the whole collapse back rather than losing a row.
    pub fn collapse_volumes_into(&self, name: &str) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let named: Option<isize> = self
            .conn
            .query_row("SELECT id FROM volumes WHERE name = ?1", [name], |row| row.get(0))
            .optional()?;
        let oldest: Option<isize> = self
            .conn
            .query_row("SELECT id FROM volumes ORDER BY id LIMIT 1", [], |row| row.get(0))
            .optional()?;
        let target = match (named, oldest) {
            (Some(id), _) => id,
            (None, Some(id)) => {
                self.conn.execute("UPDATE volumes SET name = ?1 WHERE id = ?2", rusqlite::params![name, id])?;
                id
            }
            (None, None) => self.add_volume(name)?,
        };
        let moved = self.conn.execute(
            "UPDATE chapters SET volumeid = ?1 WHERE volumeid IS NULL OR volumeid != ?1",
            [target],
        )?;
        self.conn.execute("DELETE FROM volumes WHERE id != ?1", [target])?;
        self.conn.execute("UPDATE volumes SET regenerate_epub = 0 WHERE id = ?1", [target])?;
        tx.commit()?;
        Ok(moved)
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
        // chapters.data_id and raw_data.chapter_id reference each other, so
        // break the cycle before deleting either side.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM images WHERE chapter_id = ?1", [chapter_id])?;
        tx.execute(
            "UPDATE chapters SET data_id = NULL WHERE id = ?1",
            [chapter_id],
        )?;
        tx.execute("DELETE FROM raw_data WHERE chapter_id = ?1", [chapter_id])?;
        tx.execute("DELETE FROM chapters WHERE id = ?1", [chapter_id])?;
        tx.commit()
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
        self.store_chapter_content(chapter_id, data, &[], RegenerateFlags::ChapterAndVolume)
    }

    /// Replace a chapter's HTML and images in one transaction.
    ///
    /// `flags` picks which EPUB regeneration flags are set to whether the HTML
    /// changed: the chapter's and the volume's, the chapter's alone, or neither.
    pub fn store_chapter_content(
        &self,
        chapter_id: isize,
        data: &str,
        images: &[ChapterImage],
        flags: RegenerateFlags,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

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
            .execute("DELETE FROM images WHERE chapter_id = ?1", [chapter_id])?;
        for image in images {
            self.conn.execute(
                "INSERT INTO images(chapter_id, seq, path, source_url, mime, width, height, data)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    chapter_id,
                    image.seq,
                    image.path,
                    image.source_url,
                    image.mime,
                    image.width,
                    image.height,
                    image.data
                ],
            )?;
        }

        // Upsert rather than INSERT OR REPLACE: REPLACE deletes the old row,
        // which chapters.data_id still references, and foreign keys are enforced.
        self.conn
            .prepare(
                "INSERT INTO raw_data(data, chapter_id) VALUES(?1, ?2)
                 ON CONFLICT(chapter_id) DO UPDATE SET data = excluded.data",
            )?
            .execute((data, chapter_id))?;

        let data_id: isize = self.conn.query_row(
            "SELECT id FROM raw_data WHERE chapter_id = ?1",
            [chapter_id],
            |row| row.get(0),
        )?;

        if flags != RegenerateFlags::Untouched {
            let regenerate = !existing_data.eq(data);
            self.conn
                .prepare("UPDATE chapters SET data_id = ?1, regenerate_epub = ?2 WHERE id = ?3")?
                .execute([data_id, regenerate as isize, chapter_id])?;
        } else {
            self.conn
                .prepare("UPDATE chapters SET data_id = ?1 WHERE id = ?2")?
                .execute([data_id, chapter_id])?;
        }

        if flags == RegenerateFlags::ChapterAndVolume {
            let regenerate = !existing_data.eq(data);
            let volume_id: isize = self.conn.query_row(
                "SELECT volumeid FROM chapters WHERE id = ?1",
                [chapter_id],
                |row| row.get(0),
            )?;

            self.conn
                .prepare("UPDATE volumes SET regenerate_epub = ?1 WHERE id = ?2")?
                .execute([regenerate as isize, volume_id])?;
        }

        tx.commit()
    }

    /// Get a chapter's images ordered by sequence.
    ///
    /// A database last written by a binary that predates the `images` table
    /// has none, and a query-only handle cannot create it, so a missing table
    /// means the chapter has no images.
    pub fn get_chapter_images(&self, chapter_id: isize) -> Result<Vec<ChapterImage>> {
        let has_images_table: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'images')",
            [],
            |row| row.get(0),
        )?;
        if !has_images_table {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT chapter_id, seq, path, source_url, mime, width, height, data
             FROM images WHERE chapter_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([chapter_id], |row| {
            Ok(ChapterImage {
                chapter_id: row.get(0)?,
                seq: row.get(1)?,
                path: row.get(2)?,
                source_url: row.get(3)?,
                mime: row.get(4)?,
                width: row.get(5)?,
                height: row.get(6)?,
                data: row.get(7)?,
            })
        })?;
        rows.collect()
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

    /// Get chapters that have data, in id order
    pub fn get_downloaded_chapters(&self) -> Result<Vec<Chapter>> {
        self.chapter_query(
            "SELECT id, name, uri, volumeid, data_id FROM chapters WHERE data_id IS NOT NULL ORDER BY id",
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
    use crate::test_support::CwdGuard;
    use serial_test::serial;

    use super::*;

    fn test_db() -> SourceDatabase {
        SourceDatabase::open_in_memory("test-source").unwrap()
    }

    #[test]
    #[serial]
    fn wal_mode_engages_on_file_backed_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = {
            let _cwd = CwdGuard::change_to(dir.path());
            SourceDatabase::open("t").unwrap()
        };

        let mode: String = db
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    #[serial]
    fn query_only_connection_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let ro = {
            let _cwd = CwdGuard::change_to(dir.path());
            std::fs::create_dir_all("db").unwrap();

            // Create and populate through a normal handle.
            {
                let db = SourceDatabase::open("t").unwrap();
                let vol = db.add_volume("Volume 1").unwrap();
                db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
            }

            SourceDatabase::open_query_only("t").unwrap()
        };

        // Reads work.
        let vols = ro
            .connection()
            .prepare("SELECT COUNT(*) FROM volumes")
            .unwrap()
            .query_row([], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(vols, 1);

        // Writes do not.
        let err = ro
            .connection()
            .execute("INSERT INTO volumes(name) VALUES ('Volume 2')", []);
        assert!(err.is_err(), "query_only handle must reject writes");
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

    fn image(chapter_id: isize, seq: u32, bytes: &[u8]) -> ChapterImage {
        ChapterImage {
            chapter_id, seq,
            path: format!("images/{}-{}.jpg", chapter_id, seq),
            source_url: format!("https://example.com/img/{}.jpg", seq),
            mime: "image/jpeg".into(), width: 10, height: 20, data: bytes.to_vec(),
        }
    }

    #[test]
    fn store_chapter_content_inserts_images_in_seq_order() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.store_chapter_content(id, "<html/>", &[image(id, 1, b"b"), image(id, 0, b"a")], RegenerateFlags::ChapterAndVolume).unwrap();
        let got = db.get_chapter_images(id).unwrap();
        assert_eq!(got.iter().map(|i| i.seq).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(got[0].data, b"a");
        assert_eq!(db.get_chapter_data(id).unwrap(), "<html/>");
    }

    #[test]
    fn storing_again_replaces_the_old_images() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.store_chapter_content(id, "<html>a</html>", &[image(id, 0, b"a"), image(id, 1, b"b")], RegenerateFlags::ChapterAndVolume).unwrap();
        db.store_chapter_content(id, "<html>b</html>", &[image(id, 0, b"c")], RegenerateFlags::ChapterAndVolume).unwrap();
        let got = db.get_chapter_images(id).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, b"c");
    }

    #[test]
    fn removing_a_chapter_removes_its_images() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.store_chapter_content(id, "<html/>", &[image(id, 0, b"a")], RegenerateFlags::ChapterAndVolume).unwrap();
        db.remove_chapter(id).unwrap();
        let n: i64 = db.connection().query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn unflagged_store_touches_neither_flag() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.add_chapter_data(id, "<html>old</html>").unwrap();
        db.update_generated_chapter(id, false).unwrap();
        db.update_generated_volume(vol, false).unwrap();

        db.store_chapter_content(id, "<html>new</html>", &[image(id, 0, b"a")], RegenerateFlags::Untouched).unwrap();

        assert_eq!(db.get_chapter_data(id).unwrap(), "<html>new</html>");
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());

        // And the other way: a flag that was set stays set.
        db.update_generated_chapter(id, true).unwrap();
        db.store_chapter_content(id, "<html>new</html>", &[], RegenerateFlags::Untouched).unwrap();
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 1);
    }

    #[test]
    fn flagged_store_sets_flags_on_new_and_changed_html() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        let clear = || {
            db.update_generated_chapter(id, false).unwrap();
            db.update_generated_volume(vol, false).unwrap();
        };

        db.store_chapter_content(id, "<a/>", &[], RegenerateFlags::ChapterAndVolume).unwrap();
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 1);
        assert_eq!(db.get_volumes_to_regenerate().unwrap().len(), 1);
        clear();

        db.store_chapter_content(id, "<b/>", &[], RegenerateFlags::ChapterAndVolume).unwrap();
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 1);
        assert_eq!(db.get_volumes_to_regenerate().unwrap().len(), 1);
        clear();

        db.store_chapter_content(id, "<b/>", &[], RegenerateFlags::ChapterAndVolume).unwrap();
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }

    #[test]
    fn same_html_with_different_image_bytes_does_not_flag() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.store_chapter_content(id, "<html/>", &[image(id, 0, b"a")], RegenerateFlags::ChapterAndVolume).unwrap();
        db.update_generated_chapter(id, false).unwrap();
        db.store_chapter_content(id, "<html/>", &[image(id, 0, b"zz")], RegenerateFlags::ChapterAndVolume).unwrap();
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
    }

    #[test]
    fn downloaded_chapters_are_those_with_data_in_id_order() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        for n in 1..=3 { db.add_chapter(&format!("C{}", n), &format!("https://example.com/c{}", n), vol).unwrap(); }
        let ids: Vec<isize> = db.get_empty_chapters().unwrap().iter().map(|c| c.id).collect();
        db.add_chapter_data(ids[2], "<c/>").unwrap();
        db.add_chapter_data(ids[0], "<a/>").unwrap();
        let got: Vec<isize> = db.get_downloaded_chapters().unwrap().iter().map(|c| c.id).collect();
        assert_eq!(got, vec![ids[0], ids[2]]);
    }

    #[test]
    fn collapse_moves_every_chapter_into_one_volume_and_keeps_chapter_flags() {
        let db = test_db();
        let v1 = db.add_volume("Volume 1").unwrap();
        let v2 = db.add_volume("Volume 2").unwrap();
        let v3 = db.add_volume("Volume 3").unwrap();
        db.add_chapter("C1", "https://example.com/c1", v1).unwrap();
        db.add_chapter("C2", "https://example.com/c2", v2).unwrap();
        db.add_chapter("C3", "https://example.com/c3", v3).unwrap();
        let c2 = db.get_chapters_by_volume(v2).unwrap()[0].id;
        db.update_generated_chapter(c2, true).unwrap();
        db.update_generated_volume(v1, true).unwrap();

        assert_eq!(db.collapse_volumes_into("Example Series").unwrap(), 2);

        let vols = db.get_all_volumes().unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!((vols[0].id, vols[0].name.as_str()), (v1, "Example Series"));
        let names: Vec<String> = db.get_chapters_by_volume(v1).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["C1", "C2", "C3"]);
        assert_eq!(db.get_chapters_to_regenerate().unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), [c2]);
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());

        assert_eq!(db.collapse_volumes_into("Example Series").unwrap(), 0, "idempotent");
        assert_eq!(db.get_all_volumes().unwrap().len(), 1);
    }

    #[test]
    fn collapse_creates_the_volume_on_an_empty_database() {
        let db = test_db();
        assert_eq!(db.collapse_volumes_into("Example Series").unwrap(), 0);
        let vols = db.get_all_volumes().unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].name, "Example Series");
    }

    #[test]
    fn collapse_prefers_an_existing_volume_with_the_name() {
        let db = test_db();
        let v1 = db.add_volume("Volume 1").unwrap();
        let named = db.add_volume("Example Series").unwrap();
        db.add_chapter("C1", "https://example.com/c1", v1).unwrap();
        assert_eq!(db.collapse_volumes_into("Example Series").unwrap(), 1);
        let vols = db.get_all_volumes().unwrap();
        assert_eq!((vols.len(), vols[0].id), (1, named));
        assert_eq!(db.get_chapters_by_volume(named).unwrap().len(), 1);
    }

    #[test]
    fn collapse_surfaces_a_duplicate_chapter() {
        let db = test_db();
        let v1 = db.add_volume("Volume 1").unwrap();
        let v2 = db.add_volume("Volume 2").unwrap();
        db.add_chapter("C1", "https://example.com/c1", v1).unwrap();
        db.add_chapter("C1", "https://example.com/c1", v2).unwrap();
        assert!(db.collapse_volumes_into("Example Series").is_err());
        assert_eq!(db.get_all_volumes().unwrap().len(), 2, "the transaction rolled back");
    }

    #[test]
    fn chapter_only_flags_set_the_chapter_and_leave_the_volume() {
        let db = test_db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        db.store_chapter_content(id, "<a/>", &[], RegenerateFlags::ChapterOnly).unwrap();
        assert_eq!(db.get_chapters_to_regenerate().unwrap().len(), 1);
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }

}
