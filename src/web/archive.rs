//! One zip of every EPUB a source offers, streamed as it is built.
//!
//! A finished manga series is a hundred chapter EPUBs of 10 to 40 MB each,
//! gigabytes in all, so nothing here holds more than one EPUB at a time.
//! Each is built from the database, written into a zip that streams through
//! a channel to the response body, and dropped. The zip is written in
//! streaming mode (data descriptors, no seeking) with stored entries, since
//! an EPUB is already deflated.

use std::io::Write;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::mpsc;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use crate::config::SourceConfig;
use crate::db::{Chapter, SourceDatabase, Volume};
use crate::epub::{build_chapter_epub, build_volume_epub, chapter_filename, volume_filename, EpubContext};
use crate::postprocess::ProcessorRegistry;
use crate::web::app::AppState;
use crate::web::download::{build_root, content_disposition, open, BuildError, DownloadQuery, NOT_BUILT};

const INTERNAL_ERROR_MESSAGE: &str = "Internal server error";

/// What the zip holds: one EPUB per downloaded chapter of a manga source,
/// or one per volume with a downloaded chapter of a text source.
enum Item {
    Chapter(Chapter),
    Volume(Volume, Vec<Chapter>),
}

impl Item {
    fn label(&self) -> &str {
        match self {
            Item::Chapter(c) => &c.name,
            Item::Volume(v, _) => &v.name,
        }
    }
}

/// The file a Kindle Comic Converter item is read from
fn kcc_file(source: &SourceConfig, item: &Item) -> std::path::PathBuf {
    match item {
        Item::Chapter(c) => build_root(source).join("chapters").join("kcc").join(chapter_filename(c)),
        Item::Volume(v, _) => build_root(source).join("volumes").join(volume_filename(source, v)),
    }
}

/// Blocking: the database work behind the plan. With `kcc`, a manga source
/// with `VolumeEnds` is its volumes; the files are checked before the first
/// byte, so a missing build refuses the whole archive rather than truncating it.
fn plan(source: &SourceConfig, kcc: bool) -> Result<Vec<Item>, BuildError> {
    let db = open(&source.id)?;
    let fault = |what: &str, e: rusqlite::Error| {
        eprintln!("{} failed for {}: {}", what, source.id, e);
        BuildError::Internal
    };
    let items = if source.is_manga() && kcc && source.has_volumes() {
        let mut items = Vec::new();
        for volume in db.get_all_volumes().map_err(|e| fault("volume list", e))? {
            if volume.name == source.name {
                continue;
            }
            let chapters = db.get_chapters_by_volume(volume.id).map_err(|e| fault("chapter list", e))?;
            if chapters.iter().any(|c| c.data_id.is_some()) {
                items.push(Item::Volume(volume, chapters));
            }
        }
        // Chapters with no number stay in the series volume and come singly.
        if let Some(series) = db.get_volume_by_name(&source.name).map_err(|e| fault("volume lookup", e))? {
            for chapter in db.get_chapters_by_volume(series.id).map_err(|e| fault("chapter list", e))? {
                if chapter.data_id.is_some() {
                    items.push(Item::Chapter(chapter));
                }
            }
        }
        items
    } else if source.is_manga() {
        db.get_downloaded_chapters()
            .map_err(|e| fault("chapter list", e))?
            .into_iter()
            .map(Item::Chapter)
            .collect()
    } else {
        let mut items = Vec::new();
        for volume in db.get_all_volumes().map_err(|e| fault("volume list", e))? {
            let chapters = db.get_chapters_by_volume(volume.id).map_err(|e| fault("chapter list", e))?;
            if chapters.iter().any(|c| c.data_id.is_some()) {
                items.push(Item::Volume(volume, chapters));
            }
        }
        items
    };
    if items.is_empty() {
        return Err(BuildError::NotFound("Nothing downloaded yet"));
    }
    if kcc && source.is_manga() && items.iter().any(|item| !kcc_file(source, item).is_file()) {
        return Err(BuildError::NotBuilt);
    }
    Ok(items)
}

/// `std::io::Write` into the response body's channel. `blocking_send` fails
/// once the client has gone, which stops the build.
struct ChannelWriter(mpsc::Sender<Result<Bytes, std::io::Error>>);

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .blocking_send(Ok(Bytes::copy_from_slice(buf)))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client gone"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Blocking: build each item and write it into the streamed zip. An error
/// after the first byte cannot change the status any more; the client gets
/// a truncated zip and the cause is logged.
fn write_zip(
    db: &SourceDatabase,
    source: &SourceConfig,
    items: &[Item],
    strip_colour: bool,
    kcc: bool,
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), String> {
    let registry = ProcessorRegistry::new();
    let ctx = EpubContext { source, processor_registry: &registry };
    let writer = std::io::BufWriter::with_capacity(256 * 1024, ChannelWriter(tx));
    let mut zip = zip::ZipWriter::new_stream(writer);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    for item in items {
        let built = if kcc && source.is_manga() {
            let path = kcc_file(source, item);
            std::fs::read(&path)
                .map(|bytes| crate::mail::Attachment {
                    filename: path.file_name().unwrap().to_string_lossy().into_owned(),
                    mime: "application/epub+zip".into(),
                    bytes,
                })
                .map_err(|e| format!("reading {}: {}", path.display(), e))?
        } else {
            match item {
                Item::Chapter(c) => build_chapter_epub(db, c, &ctx, strip_colour),
                Item::Volume(v, chapters) => build_volume_epub(db, v, chapters, &ctx, strip_colour),
            }
            .map_err(|e| format!("building {}: {}", item.label(), e))?
        };
        zip.start_file(&built.filename, options).map_err(|e| e.to_string())?;
        zip.write_all(&built.bytes).map_err(|e| e.to_string())?;
    }
    let mut inner = zip.finish().map_err(|e| e.to_string())?;
    inner.flush().map_err(|e| e.to_string())
}

/// `GET /source/{source_id}/epubs.zip`, `?stripped=1` for a text source's
/// colour-stripped volumes.
pub async fn source_epubs_zip(
    State(state): State<Arc<AppState>>,
    AxumPath(source_id): AxumPath<String>,
    Query(query): Query<DownloadQuery>,
) -> Response {
    // One snapshot for the whole request; see `LiveRegistry::snapshot`.
    let registry = state.registry.snapshot();
    let Some(entry) = registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    let source = entry.config.clone();
    let strip_colour = query.strip_colour() && !source.is_manga();
    let kcc = registry.kcc_builds() && source.is_manga();

    // One permit for the whole archive: it is one long build, not many.
    let permit = Arc::clone(&state.epub_permits).acquire_owned().await;
    let Ok(permit) = permit else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Shutting down").into_response();
    };

    // Plan before the first byte, so an empty source is a 404 and not an
    // empty archive.
    let planned = {
        let source = source.clone();
        tokio::task::spawn_blocking(move || plan(&source, kcc)).await
    };
    let items = match planned {
        Ok(Ok(items)) => items,
        Ok(Err(BuildError::NotFound(msg))) => return (StatusCode::NOT_FOUND, msg).into_response(),
        Ok(Err(BuildError::NotBuilt)) => return (StatusCode::SERVICE_UNAVAILABLE, NOT_BUILT).into_response(),
        Ok(Err(BuildError::Internal)) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response()
        }
        Err(e) => {
            eprintln!("zip plan task panicked: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response();
        }
    };

    let filename = format!(
        "{}{}.zip",
        source.name,
        if strip_colour { " (no colour)" } else { "" }
    );
    let (tx, rx) = mpsc::channel(8);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let db = match open(&source.id) {
            Ok(db) => db,
            Err(_) => return,
        };
        if let Err(e) = write_zip(&db, &source, &items, strip_colour, kcc, tx) {
            eprintln!("zip of {} stopped: {}", source.id, e);
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    });

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition(&filename)),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;

    use axum::extract::{Path as AxumPath, Query, State};
    use axum::http::StatusCode;

    use super::source_epubs_zip;
    use crate::config::{SourceConfig, SourceKind};
    use crate::db::SourceDatabase;
    use crate::test_support::CwdGuard;
    use crate::web::app::AppState;
    use crate::web::download::DownloadQuery;

    async fn fetch(state: &Arc<AppState>, stripped: Option<&str>) -> (StatusCode, String, Vec<u8>) {
        let response = source_epubs_zip(
            State(Arc::clone(state)),
            AxumPath("test-source".to_string()),
            Query(DownloadQuery { stripped: stripped.map(str::to_string) }),
        )
        .await;
        let status = response.status();
        let disposition = response
            .headers()
            .get("content-disposition")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, disposition, body.to_vec())
    }

    fn entries(bytes: &[u8]) -> Vec<(String, zip::CompressionMethod, Vec<u8>)> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut f = archive.by_index(i).unwrap();
                let mut data = Vec::new();
                f.read_to_end(&mut data).unwrap();
                (f.name().unwrap().into_owned(), f.compression(), data)
            })
            .collect()
    }

    fn state(kind: SourceKind) -> Arc<AppState> {
        Arc::new(AppState::for_test_with_sources(vec![SourceConfig {
            id: "test-source".into(),
            name: "Example Series".into(),
            enabled: true,
            kind,
            ..Default::default()
        }]))
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_manga_zip_holds_one_epub_per_downloaded_chapter() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        let downloaded = {
            let db = SourceDatabase::open("test-source").unwrap();
            let vol = db.add_volume("Example Series").unwrap();
            db.add_chapter("Chapter 1", "https://example.com/c1", vol).unwrap();
            db.add_chapter("Chapter 2", "https://example.com/c2", vol).unwrap();
            let chapters = db.get_chapters_by_volume(vol).unwrap();
            db.add_chapter_data(chapters[0].id, "<p>one</p>").unwrap();
            chapters[0].id
        };

        let (status, disposition, body) = fetch(&state(SourceKind::MangaWordpress), Some("1")).await;

        assert_eq!(status, StatusCode::OK);
        assert!(disposition.contains("Example Series.zip"), "{}", disposition);
        assert!(!disposition.contains("no colour"), "manga has no stripped variant: {}", disposition);
        let entries = entries(&body);
        assert_eq!(entries.len(), 1, "{:?}", entries.iter().map(|e| &e.0).collect::<Vec<_>>());
        assert_eq!(entries[0].0, format!("{}(Chapter 1).epub", downloaded));
        assert_eq!(entries[0].1, zip::CompressionMethod::Stored);
        assert!(entries[0].2.starts_with(b"PK"), "entry is an EPUB (zip) itself");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_text_zip_holds_one_epub_per_volume_with_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        {
            let db = SourceDatabase::open("test-source").unwrap();
            let v1 = db.add_volume("Volume 1").unwrap();
            let v2 = db.add_volume("Volume 2").unwrap();
            db.add_chapter("1.01", "https://example.com/c1", v1).unwrap();
            db.add_chapter("2.01", "https://example.com/c2", v2).unwrap();
            let id = db.get_chapters_by_volume(v1).unwrap()[0].id;
            db.add_chapter_data(id, r#"<p><span style="color:#ff0000">red</span></p>"#).unwrap();
        }

        let (status, disposition, body) = fetch(&state(SourceKind::Text), Some("1")).await;

        assert_eq!(status, StatusCode::OK);
        assert!(disposition.contains("Example Series (no colour).zip"), "{}", disposition);
        let entries = entries(&body);
        assert_eq!(entries.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(), ["Volume 1.epub"]);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_zip_is_refused_when_nothing_is_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        {
            let db = SourceDatabase::open("test-source").unwrap();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("1.01", "https://example.com/c1", vol).unwrap();
        }

        let (status, _, body) = fetch(&state(SourceKind::Text), None).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(std::str::from_utf8(&body).unwrap(), "Nothing downloaded yet");
    }

    fn kcc_state(volume_ends: Vec<f64>) -> Arc<AppState> {
        Arc::new(AppState::for_test_with_sources_and_kcc(vec![SourceConfig {
            id: "test-source".into(),
            name: "Example Series".into(),
            enabled: true,
            kind: SourceKind::MangaWordpress,
            volume_ends,
            ..Default::default()
        }], true))
    }

    /// With Kindle Comic Converter and `VolumeEnds`, the zip is the volume
    /// files; a missing one refuses the whole archive up front.
    #[tokio::test]
    #[serial_test::serial]
    async fn the_kcc_zip_holds_volume_files_and_refuses_when_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        {
            let db = SourceDatabase::open("test-source").unwrap();
            db.add_volume("Example Series").unwrap();
            let v1 = db.add_volume("Volume 1").unwrap();
            let v2 = db.add_volume("Volume 2").unwrap();
            db.add_chapter("Chapter 1", "https://example.com/c1", v1).unwrap();
            db.add_chapter("Chapter 2", "https://example.com/c2", v2).unwrap();
            let id = db.get_chapters_by_volume(v1).unwrap()[0].id;
            db.add_chapter_data(id, "<p>one</p>").unwrap();
        }
        let state = kcc_state(vec![1.0]);

        let (status, _, body) = fetch(&state, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(std::str::from_utf8(&body).unwrap(), crate::web::download::NOT_BUILT);

        std::fs::create_dir_all("build/test-source/volumes").unwrap();
        std::fs::write("build/test-source/volumes/Example Series Volume 1.epub", b"fakevol").unwrap();
        let (status, _, body) = fetch(&state, None).await;
        assert_eq!(status, StatusCode::OK);
        let entries = entries(&body);
        assert_eq!(entries.iter().map(|e| (e.0.as_str(), e.2.as_slice())).collect::<Vec<_>>(), [("Example Series Volume 1.epub", b"fakevol".as_slice())], "only Volume 1 has a download; Volume 2 is pending");
    }

    /// Without `VolumeEnds` the Kindle Comic Converter zip is the chapter files.
    #[tokio::test]
    #[serial_test::serial]
    async fn the_kcc_zip_of_a_flat_source_holds_chapter_files() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        let id = {
            let db = SourceDatabase::open("test-source").unwrap();
            let series = db.add_volume("Example Series").unwrap();
            db.add_chapter("Chapter 1", "https://example.com/c1", series).unwrap();
            let id = db.get_chapters_by_volume(series).unwrap()[0].id;
            db.add_chapter_data(id, "<p>one</p>").unwrap();
            id
        };
        let state = kcc_state(vec![]);
        std::fs::create_dir_all("build/test-source/chapters/kcc").unwrap();
        std::fs::write(format!("build/test-source/chapters/kcc/{}(Chapter 1).epub", id), b"fakech").unwrap();

        let (status, _, body) = fetch(&state, None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(entries(&body).iter().map(|e| e.0.clone()).collect::<Vec<_>>(), [format!("{}(Chapter 1).epub", id)]);
    }
}
