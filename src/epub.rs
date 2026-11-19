use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use image::io::Reader as ImageReader;
use image::{DynamicImage, Rgba, RgbaImage};
use imageproc::drawing::draw_text_mut;
use rusttype::{Font, Scale};
use std::{
    io::{Cursor, Write},
    path::Path,
};

use crate::config::{Config, SourceConfig};
use crate::db::{Chapter, SourceDatabase, Volume};
use crate::mail::{send_epubs, Attachment};
use crate::postprocess::ProcessorRegistry;

/// Context for EPUB generation containing source metadata and processors
pub struct EpubContext<'a> {
    pub source: &'a SourceConfig,
    pub processor_registry: &'a ProcessorRegistry,
}

/// Load font for cover text rendering
fn load_font() -> Option<Font<'static>> {
    const FONT_DATA: &[u8] = include_bytes!("font/RobotoSlab-VariableFont_wght.ttf");
    Font::try_from_bytes(FONT_DATA)
}

/// Calculate the width of rendered text
fn text_width(font: &Font, scale: Scale, text: &str) -> i32 {
    let glyphs: Vec<_> = font
        .layout(text, scale, rusttype::point(0.0, 0.0))
        .collect();
    if glyphs.is_empty() {
        return 0;
    }
    let min_x = glyphs
        .first()
        .and_then(|g| g.pixel_bounding_box())
        .map(|bb| bb.min.x)
        .unwrap_or(0);
    let max_x = glyphs
        .last()
        .and_then(|g| g.pixel_bounding_box())
        .map(|bb| bb.max.x)
        .unwrap_or(0);
    max_x - min_x
}

/// Draw a semi-transparent overlay rectangle
fn draw_overlay(img: &mut RgbaImage, y_start: u32, height: u32, opacity: u8) {
    let width = img.width();
    for y in y_start..(y_start + height).min(img.height()) {
        for x in 0..width {
            let pixel = img.get_pixel_mut(x, y);
            // Blend with dark overlay
            let alpha = opacity as f32 / 255.0;
            pixel[0] = ((pixel[0] as f32) * (1.0 - alpha)) as u8;
            pixel[1] = ((pixel[1] as f32) * (1.0 - alpha)) as u8;
            pixel[2] = ((pixel[2] as f32) * (1.0 - alpha)) as u8;
        }
    }
}

/// Generate cover image with title text overlay
fn generate_cover_with_text(
    source: &SourceConfig,
    series_title: &str,
    subtitle: &str,
) -> Option<Vec<u8>> {
    let cover_path = source.metadata.cover_image.as_ref()?;

    let img = ImageReader::open(cover_path).ok()?.decode().ok()?;
    let font = load_font()?;

    let mut img = img.to_rgba8();
    let (width, height) = (img.width(), img.height());

    // Calculate font sizes based on image dimensions
    let title_scale = Scale::uniform((width as f32 * 0.08).clamp(24.0, 72.0));
    let subtitle_scale = Scale::uniform((width as f32 * 0.05).clamp(16.0, 48.0));

    // Calculate text positions (centered, bottom portion of image)
    let title_width = text_width(&font, title_scale, series_title);
    let subtitle_width = text_width(&font, subtitle_scale, subtitle);

    let title_x = ((width as i32 - title_width) / 2).max(10);
    let subtitle_x = ((width as i32 - subtitle_width) / 2).max(10);

    // Position text in the bottom third of the image
    let text_area_height = (height as f32 * 0.2) as u32;
    let overlay_y = height - text_area_height - 20;
    let title_y = height - text_area_height;
    let subtitle_y = title_y + (title_scale.y as u32) + 10;

    // Draw semi-transparent overlay for text readability
    draw_overlay(&mut img, overlay_y, text_area_height + 40, 180);

    // Draw title text (white)
    let white = Rgba([255u8, 255u8, 255u8, 255u8]);
    draw_text_mut(
        &mut img,
        white,
        title_x,
        title_y as i32,
        title_scale,
        &font,
        series_title,
    );
    draw_text_mut(
        &mut img,
        white,
        subtitle_x,
        subtitle_y as i32,
        subtitle_scale,
        &font,
        subtitle,
    );

    // Encode to PNG
    let mut img_bytes = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_to(
            &mut Cursor::new(&mut img_bytes),
            image::ImageOutputFormat::Png,
        )
        .ok()?;

    Some(img_bytes)
}

/// Replace path separators so chapter/volume names can be used in filenames
/// and zip-internal content paths.
fn sanitize_filename(name: &str) -> String {
    name.replace(['/', '\\'], "-")
}

fn load_stylesheet() -> &'static str {
    include_str!("assets/style.css")
}

fn process_chapter_data(raw_data: &str, ctx: &EpubContext, strip_colour: bool) -> String {
    // Always apply mrsha-write processor
    let mut processed = ctx.processor_registry.apply(raw_data, "mrsha-write");

    // Optionally apply strip-colour
    if strip_colour {
        processed = ctx.processor_registry.apply(&processed, "strip-colour");
    }

    processed
}

/// Add a chapter's stored images as EPUB resources at the relative paths
/// its HTML references.
fn add_chapter_images(
    epub: &mut EpubBuilder<ZipLibrary>,
    db: &SourceDatabase,
    chapter_id: isize,
) -> Result<(), Box<dyn std::error::Error>> {
    for img in db.get_chapter_images(chapter_id)? {
        epub.add_resource(&img.path, img.data.as_slice(), &img.mime)?;
    }
    Ok(())
}

/// Build a single-chapter EPUB in memory. Touches no filesystem path.
pub fn build_chapter_epub(
    db: &SourceDatabase,
    chapter: &Chapter,
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Attachment, Box<dyn std::error::Error>> {
    let mut output = Vec::<u8>::new();
    let safe_name = sanitize_filename(&chapter.name);

    let mut epub = EpubBuilder::new(ZipLibrary::new()?)?;
    epub.metadata("author", &ctx.source.metadata.author)?;
    epub.metadata("lang", "en")?;
    epub.metadata("title", &chapter.name)?;
    epub.metadata("generator", "rsauvehoover/wandering-inn-scraper")?;

    let images = db.get_chapter_images(chapter.id)?;
    let mut pages = images.as_slice();
    if let Some(img_bytes) = generate_cover_with_text(ctx.source, &ctx.source.name, &chapter.name) {
        epub.add_cover_image(
            format!("{}({}).png", chapter.id, safe_name),
            img_bytes.as_slice(),
            "image/png",
        )?;
    } else if ctx.source.is_manga() {
        // No configured cover: a manga chapter's first page stands in for it,
        // added once, at the path its page references.
        if let Some((first, rest)) = images.split_first() {
            epub.add_cover_image(&first.path, first.data.as_slice(), &first.mime)?;
            pages = rest;
        }
    }
    epub.stylesheet(load_stylesheet().as_bytes())?;

    let raw_data = db.get_chapter_data(chapter.id)?;
    let processed_data = process_chapter_data(&raw_data, ctx, strip_colour);

    for img in pages {
        epub.add_resource(&img.path, img.data.as_slice(), &img.mime)?;
    }
    epub.add_content(
        EpubContent::new(
            format!("{}({}).xhtml", &chapter.id, &safe_name),
            processed_data.as_bytes(),
        )
        .title(&chapter.name),
    )?;

    epub.generate(&mut output)?;

    Ok(Attachment {
        filename: format!("{}({}).epub", &chapter.id, &safe_name),
        mime: String::from("application/epub+zip"),
        bytes: output,
    })
}

fn generate_chapter(
    db: &SourceDatabase,
    chapter: &Chapter,
    output_dir: &Path,
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Attachment, Box<dyn std::error::Error>> {
    let attachment = build_chapter_epub(db, chapter, ctx, strip_colour)?;
    std::fs::create_dir_all(output_dir.join("individual"))?;
    let mut file = std::fs::File::create(output_dir.join("individual").join(&attachment.filename))?;
    file.write_all(&attachment.bytes)?;
    Ok(attachment)
}

fn generate_chapters(
    db: &SourceDatabase,
    chapters: &[Chapter],
    output_dir: &Path,
    ctx: &EpubContext,
    strip_colour: bool,
    combined: bool,
    keep_attachments: bool,
) -> Result<Vec<Attachment>, Box<dyn std::error::Error>> {
    if !combined {
        let mut attachments = Vec::<Attachment>::new();
        for chapter in chapters {
            let attachment = generate_chapter(db, chapter, output_dir, ctx, strip_colour)?;
            if keep_attachments {
                attachments.push(attachment);
            }
            db.update_generated_chapter(chapter.id, false)?;
        }
        return Ok(attachments);
    }

    std::fs::create_dir_all(output_dir.join("combined"))?;

    if chapters.is_empty() {
        return Ok(Vec::<Attachment>::default());
    }

    let mut combined_output = Vec::<u8>::new();
    let last_chapter = chapters.last().unwrap();
    let first_safe_name = sanitize_filename(&chapters[0].name);
    let last_safe_name = sanitize_filename(&last_chapter.name);
    let mut combined_epub = EpubBuilder::new(ZipLibrary::new()?)?;
    combined_epub.metadata("author", &ctx.source.metadata.author)?;
    combined_epub.metadata("lang", "en")?;
    combined_epub.metadata(
        "title",
        format!(
            "{} Chapters {}-{}",
            ctx.source.name, chapters[0].name, last_chapter.name
        ),
    )?;
    combined_epub.metadata("generator", "rsauvehoover/wandering-inn-scraper")?;
    combined_epub.stylesheet(load_stylesheet().as_bytes())?;

    let chapters_subtitle = format!("{} - {}", chapters[0].name, last_chapter.name);
    if let Some(img_bytes) =
        generate_cover_with_text(ctx.source, &ctx.source.name, &chapters_subtitle)
    {
        combined_epub.add_cover_image(
            format!(
                "{}({})-{}({}).png",
                chapters[0].id, first_safe_name, last_chapter.id, last_safe_name
            ),
            img_bytes.as_slice(),
            "image/png",
        )?;
    }
    combined_epub.inline_toc();

    let mut attachments = Vec::<Attachment>::new();

    for chapter in chapters {
        let raw_data = db.get_chapter_data(chapter.id)?;
        let processed_data = process_chapter_data(&raw_data, ctx, strip_colour);

        add_chapter_images(&mut combined_epub, db, chapter.id)?;
        combined_epub.add_content(
            EpubContent::new(
                format!("{}({}).xhtml", chapter.id, sanitize_filename(&chapter.name)),
                processed_data.as_bytes(),
            )
            .title(&chapter.name),
        )?;
        let attachment = generate_chapter(db, chapter, output_dir, ctx, strip_colour)?;
        if keep_attachments {
            attachments.push(attachment);
        }
        db.update_generated_chapter(chapter.id, false)?;
    }

    combined_epub.generate(&mut combined_output)?;

    let mut file = std::fs::File::create(output_dir.join("combined").join(format!(
        "{}({})-{}({}).epub",
        chapters[0].id, first_safe_name, last_chapter.id, last_safe_name
    )))?;
    file.write_all(&combined_output)?;
    Ok(attachments)
}

/// Build a volume EPUB in memory. Touches no filesystem path.
pub fn build_volume_epub(
    db: &SourceDatabase,
    volume: &Volume,
    chapters: &[Chapter],
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Attachment, Box<dyn std::error::Error>> {
    let mut output = Vec::<u8>::new();

    let safe_volume_name = sanitize_filename(&volume.name);
    let mut epub = EpubBuilder::new(ZipLibrary::new()?)?;
    epub.metadata("author", &ctx.source.metadata.author)?;
    epub.metadata("lang", "en")?;
    epub.metadata("title", format!("{} {}", ctx.source.name, &volume.name))?;
    epub.metadata("generator", "rsauvehoover/wandering-inn-scraper")?;
    epub.stylesheet(load_stylesheet().as_bytes())?;

    if let Some(img_bytes) = generate_cover_with_text(ctx.source, &ctx.source.name, &volume.name) {
        epub.add_cover_image(
            format!("{}.png", &safe_volume_name),
            img_bytes.as_slice(),
            "image/png",
        )?;
    }

    epub.inline_toc();

    let last_chapter_id = chapters.last().ok_or("No chapters found")?.id;
    for chapter in chapters {
        let raw_data = match db.get_chapter_data(chapter.id) {
            Err(rusqlite::Error::QueryReturnedNoRows) if chapter.id == last_chapter_id => {
                println!(
                    "  Failed to fetch data for last chapter ({}), assuming unreleased content.",
                    chapter.name
                );
                continue;
            }
            Err(e) => return Err(e.into()),
            Ok(data) => data,
        };

        let processed_data = process_chapter_data(&raw_data, ctx, strip_colour);

        add_chapter_images(&mut epub, db, chapter.id)?;
        epub.add_content(
            EpubContent::new(
                format!("{}({}).xhtml", chapter.id, sanitize_filename(&chapter.name)),
                processed_data.as_bytes(),
            )
            .title(&chapter.name),
        )?;
    }

    epub.generate(&mut output)?;

    Ok(Attachment {
        filename: format!("{}.epub", safe_volume_name),
        mime: String::from("application/epub+zip"),
        bytes: output,
    })
}

/// Build a volume EPUB and persist it under `output_dir`. CLI path only.
fn generate_volume(
    db: &SourceDatabase,
    volume: &Volume,
    chapters: &[Chapter],
    output_dir: &Path,
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Attachment, Box<dyn std::error::Error>> {
    let attachment = build_volume_epub(db, volume, chapters, ctx, strip_colour)?;
    std::fs::create_dir_all(output_dir)?;
    let mut file = std::fs::File::create(output_dir.join(&attachment.filename))?;
    file.write_all(&attachment.bytes)?;
    Ok(attachment)
}

/// Generate EPUBs for a specific source, then mail them.
pub async fn generate_epubs_for_source(
    db: &SourceDatabase,
    build_dir: &Path,
    config: &Config,
    source: &SourceConfig,
    processor_registry: &ProcessorRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
    let built = build_epubs_for_source(db, build_dir, config, source, processor_registry)?;
    send_epubs(
        &config.mail,
        &source.id,
        &built.vols,
        &built.vols_stripped,
        &built.chaps,
        &built.chaps_stripped,
    )
    .await;
    Ok(())
}

/// The EPUBs one generation run built and kept for mailing.
pub(crate) struct GeneratedEpubs {
    pub vols: Vec<Attachment>,
    pub vols_stripped: Vec<Attachment>,
    pub chaps: Vec<Attachment>,
    pub chaps_stripped: Vec<Attachment>,
}

/// Write a source's pending EPUBs to disk and clear their flags. Sends
/// nothing. The bytes are kept only when some destination receives the
/// source; otherwise every list comes back empty, so a first scrape of a
/// long series does not hold every EPUB in memory for no recipient.
pub(crate) fn build_epubs_for_source(
    db: &SourceDatabase,
    build_dir: &Path,
    config: &Config,
    source: &SourceConfig,
    processor_registry: &ProcessorRegistry,
) -> Result<GeneratedEpubs, Box<dyn std::error::Error>> {
    let wanted = config.mail.destinations.iter().any(|d| d.receives_source(&source.id));
    let ctx = EpubContext {
        source,
        processor_registry,
    };

    // Create source-specific output directory
    let source_dir = build_dir.join(&source.id);

    let mut vols = Vec::<Attachment>::new();
    let mut vols_stripped = Vec::<Attachment>::new();
    let mut chaps = Vec::<Attachment>::new();
    let mut chaps_stripped = Vec::<Attachment>::new();

    if config.epub_gen.volumes && source.is_manga() {
        // A manga source's single volume is never built: its chapters are
        // sent one by one. Clear its flag so it does not sit there forever.
        println!("({}) Skipping volume generation: manga chapters are sent individually", source.id);
        for volume in db.get_volumes_to_regenerate()? {
            db.update_generated_volume(volume.id, false)?;
        }
    } else if config.epub_gen.volumes {
        let volumes = db.get_volumes_to_regenerate()?;

        if volumes.is_empty() {
            println!("({}) No volumes to generate", source.id);
        } else {
            println!(
                "({}) Generating epubs for {} volumes",
                source.id,
                volumes.len()
            );
        }

        for volume in volumes {
            println!("({}) Generating epub for {}", source.id, volume.name);
            let chapters = db.get_chapters_by_volume(volume.id)?;
            if config.epub_gen.strip_colour {
                let stripped = generate_volume(
                    db,
                    &volume,
                    &chapters,
                    &source_dir.join("volumes_stripped_colour"),
                    &ctx,
                    true,
                )?;
                if wanted {
                    vols_stripped.push(stripped);
                }
            }
            let plain = generate_volume(
                db,
                &volume,
                &chapters,
                &source_dir.join("volumes"),
                &ctx,
                false,
            )?;
            if wanted {
                vols.push(plain);
            }
            db.update_generated_volume(volume.id, false)?;
        }
    } else {
        println!("({}) Skipping volume generation", source.id);
    }

    if config.epub_gen.chapters {
        let chapters = db.get_chapters_to_regenerate()?;
        if chapters.is_empty() {
            println!("({}) No chapters to generate", source.id);
        } else {
            println!(
                "({}) Generating epubs for {} chapters",
                source.id,
                chapters.len()
            );
            // Manga sources get plain chapter EPUBs only: no stripped variant, no combined EPUB.
            let manga = source.is_manga();
            if config.epub_gen.strip_colour && !manga {
                chaps_stripped = generate_chapters(
                    db,
                    &chapters,
                    &source_dir.join("chapters_stripped_colour"),
                    &ctx,
                    true,
                    true,
                    wanted,
                )?;
            }
            chaps = generate_chapters(
                db,
                &chapters,
                &source_dir.join("chapters"),
                &ctx,
                false,
                !manga,
                wanted,
            )?;
        }
    } else {
        println!("({}) Skipping chapter generation", source.id);
    }

    Ok(GeneratedEpubs {
        vols,
        vols_stripped,
        chaps,
        chaps_stripped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EpubGenConfig, SourceKind};
    use crate::db::{ChapterImage, SourceDatabase};
    use crate::sources::manga::pages_html;
    use serial_test::serial;

    fn fixture_db() -> (SourceDatabase, Volume, Vec<Chapter>) {
        let db = SourceDatabase::open_in_memory("test-source").unwrap();
        let vol_id = db.add_volume("Volume 1").unwrap();
        db.add_chapter("Chapter One", "https://example.com/c1", vol_id)
            .unwrap();
        let chapters = db.get_chapters_by_volume(vol_id).unwrap();
        db.add_chapter_data(chapters[0].id, "<h1>Chapter One</h1><p>body text</p>")
            .unwrap();
        let chapters = db.get_chapters_by_volume(vol_id).unwrap();
        (
            db,
            Volume {
                id: vol_id,
                name: "Volume 1".to_string(),
            },
            chapters,
        )
    }

    // `build_volume_epub` takes no path parameter, so the no-write guarantee
    // is primarily structural: there is nothing to write through today. This
    // test guards against that guarantee being eroded by a *relative*-path
    // write creeping back in (e.g. `File::create("out.epub")` or a stray
    // `create_dir_all`) — it does not, and cannot, prove the function writes
    // nowhere on the filesystem in general, since an absolute path is not
    // ruled out by this check.
    #[serial]
    #[test]
    fn build_volume_epub_writes_no_files_in_cwd() {
        let (db, volume, chapters) = fixture_db();
        let source = crate::config::SourceConfig::default();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext {
            source: &source,
            processor_registry: &registry,
        };

        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let result = build_volume_epub(&db, &volume, &chapters, &ctx, false);

        std::env::set_current_dir(original).unwrap();

        let after: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        let attachment = result.unwrap();
        assert_eq!(after.len(), 0, "build must not create files in cwd");
        assert_eq!(attachment.filename, "Volume 1.epub");
        assert!(!attachment.bytes.is_empty());
        // EPUBs are ZIP archives.
        assert_eq!(&attachment.bytes[0..2], b"PK");
    }

    #[test]
    fn build_chapter_epub_returns_bytes() {
        let (db, _volume, chapters) = fixture_db();
        let source = crate::config::SourceConfig::default();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext {
            source: &source,
            processor_registry: &registry,
        };

        let attachment = build_chapter_epub(&db, &chapters[0], &ctx, false).unwrap();
        assert!(attachment.filename.ends_with(".epub"));
        assert_eq!(&attachment.bytes[0..2], b"PK");
        assert_eq!(attachment.mime, "application/epub+zip");
    }

    /// The EPUB container spec requires `mimetype` as the first archive
    /// entry, uncompressed. The vendored epub-builder's zip wrapper is what
    /// guarantees it, so pin it here.
    #[test]
    fn chapter_epub_starts_with_a_stored_mimetype_entry() {
        let (db, _volume, chapters) = fixture_db();
        let source = crate::config::SourceConfig::default();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext {
            source: &source,
            processor_registry: &registry,
        };

        let attachment = build_chapter_epub(&db, &chapters[0], &ctx, false).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(attachment.bytes)).unwrap();
        let mut first = z.by_index(0).unwrap();
        assert_eq!(first.name().unwrap(), "mimetype");
        assert_eq!(first.compression(), zip::CompressionMethod::Stored);
        let mut body = String::new();
        std::io::Read::read_to_string(&mut first, &mut body).unwrap();
        assert_eq!(body, "application/epub+zip");
    }

    /// The web process opens databases query-only and never runs the schema
    /// migration, so a database last written by a binary that predates the
    /// `images` table must still build: no table means no images.
    #[serial]
    #[test]
    fn build_chapter_epub_works_on_database_without_images_table() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = crate::test_support::CwdGuard::change_to(dir.path());
        std::fs::create_dir("db").unwrap();
        {
            let conn = rusqlite::Connection::open(SourceDatabase::path_for("test-source")).unwrap();
            conn.execute_batch(
                "CREATE TABLE volumes(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    regenerate_epub INTEGER DEFAULT 0 CHECK(regenerate_epub IN (0, 1)),
                    UNIQUE(name)
                );
                CREATE TABLE chapters(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    uri TEXT NOT NULL,
                    volumeid INTEGER,
                    data_id INTEGER,
                    regenerate_epub INTEGER DEFAULT 0 CHECK(regenerate_epub IN (0, 1)),
                    FOREIGN KEY(data_id) REFERENCES raw_data(id),
                    FOREIGN KEY(volumeid) REFERENCES volumes(id),
                    UNIQUE(name, uri, volumeid)
                );
                CREATE TABLE raw_data(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    chapter_id INTEGER,
                    data TEXT,
                    FOREIGN KEY(chapter_id) REFERENCES chapters(id),
                    UNIQUE(chapter_id)
                );
                INSERT INTO volumes(id, name) VALUES(1, 'Volume 1');
                INSERT INTO chapters(id, name, uri, volumeid)
                    VALUES(1, 'Chapter One', 'https://example.com/c1', 1);
                INSERT INTO raw_data(id, chapter_id, data) VALUES(1, 1, '<p>body</p>');
                UPDATE chapters SET data_id = 1 WHERE id = 1;",
            )
            .unwrap();
        }

        let db = SourceDatabase::open_query_only("test-source").unwrap();
        let source = crate::config::SourceConfig::default();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext {
            source: &source,
            processor_registry: &registry,
        };

        assert!(db.get_chapter_images(1).unwrap().is_empty());
        let chapters = db.get_chapters_by_volume(1).unwrap();
        let attachment = build_chapter_epub(&db, &chapters[0], &ctx, false).unwrap();
        assert_eq!(&attachment.bytes[0..2], b"PK");
        let volume = Volume { id: 1, name: "Volume 1".to_string() };
        build_volume_epub(&db, &volume, &chapters, &ctx, false).unwrap();
    }

    #[test]
    fn sanitize_filename_replaces_path_separators() {
        assert_eq!(
            sanitize_filename("Chapter 1 — Title// with slashes"),
            "Chapter 1 — Title-- with slashes"
        );
        assert_eq!(sanitize_filename("back\\slash"), "back-slash");
        assert_eq!(sanitize_filename("plain name"), "plain name");
    }

    #[serial]
    #[test]
    fn assets_load_without_filesystem_access() {
        // Changing to a directory with no src/ must not panic. This is the
        // regression guard for the cwd-relative .unwrap() that forced deployments
        // to symlink a src/ directory next to the working directory.
        let tmp = std::env::temp_dir();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let css = load_stylesheet();
        let font = load_font();

        std::env::set_current_dir(original).unwrap();

        assert!(!css.is_empty(), "stylesheet must be embedded");
        assert!(font.is_some(), "font must be embedded");
    }

    fn read_zip_entry(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut f = z.by_name(name).ok()?;
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut out).unwrap();
        Some(out)
    }

    fn store_with_image(db: &SourceDatabase, id: isize, byte: u8) -> String {
        let path = format!("images/{}-0.jpg", id);
        let html = format!(
            r#"<html><body><p>t</p><img src="{}" alt="" style="s"/></body></html>"#,
            path
        );
        let img = ChapterImage {
            chapter_id: id,
            seq: 0,
            path: path.clone(),
            source_url: "https://example.com/a.jpg".into(),
            mime: "image/jpeg".into(),
            width: 1,
            height: 1,
            data: vec![byte; 3],
        };
        db.store_chapter_content(id, &html, &[img], crate::db::RegenerateFlags::ChapterAndVolume).unwrap();
        path
    }

    #[test]
    fn chapter_epub_contains_its_images_at_the_referenced_path() {
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        let id = db.get_empty_chapters().unwrap()[0].id;
        let path = store_with_image(&db, id, 9);
        let chapters = db.get_chapters_by_volume(vol).unwrap();
        let source = SourceConfig::default();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext { source: &source, processor_registry: &registry };

        let epub = build_chapter_epub(&db, &chapters[0], &ctx, false).unwrap();

        assert_eq!(
            read_zip_entry(&epub.bytes, &format!("OEBPS/{}", path)),
            Some(vec![9, 9, 9])
        );
        let opf =
            String::from_utf8(read_zip_entry(&epub.bytes, "OEBPS/content.opf").unwrap()).unwrap();
        assert!(opf.contains(&path) && opf.contains("image/jpeg"), "{}", opf);
        let xhtml = String::from_utf8(
            read_zip_entry(&epub.bytes, &format!("OEBPS/{}(C1).xhtml", id)).unwrap(),
        )
        .unwrap();
        assert!(xhtml.contains(&format!("src=\"{}\"", path)));
    }

    #[test]
    fn volume_epub_contains_images_of_every_chapter() {
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        db.add_chapter("C2", "https://example.com/c2", vol).unwrap();
        let ids: Vec<isize> = db.get_empty_chapters().unwrap().iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 2);
        let p0 = store_with_image(&db, ids[0], 1);
        let p1 = store_with_image(&db, ids[1], 2);
        let chapters = db.get_chapters_by_volume(vol).unwrap();
        let volume = Volume { id: vol, name: "Volume 1".to_string() };
        let source = SourceConfig::default();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext { source: &source, processor_registry: &registry };

        let epub = build_volume_epub(&db, &volume, &chapters, &ctx, false).unwrap();

        assert_eq!(read_zip_entry(&epub.bytes, &format!("OEBPS/{}", p0)), Some(vec![1; 3]));
        assert_eq!(read_zip_entry(&epub.bytes, &format!("OEBPS/{}", p1)), Some(vec![2; 3]));
    }

    fn one_by_one_png() -> Vec<u8> {
        use image::ImageEncoder;
        let img = image::RgbImage::new(1, 1);
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 1, 1, image::ColorType::Rgb8)
            .unwrap();
        out
    }

    fn manga_db_with_two_chapters() -> (SourceDatabase, SourceConfig) {
        let source = SourceConfig {
            id: "manga-test".into(),
            name: "Example Series".into(),
            kind: SourceKind::MangaTcb,
            ..Default::default()
        };
        let db = SourceDatabase::open_in_memory(&source.id).unwrap();
        let vol = db.add_volume("Example Series").unwrap();
        db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
        db.add_chapter("C2", "https://example.com/c2", vol).unwrap();
        for chapter in db.get_chapters_by_volume(vol).unwrap() {
            let path = format!("images/{}-0.png", chapter.id);
            let image = ChapterImage {
                chapter_id: chapter.id,
                seq: 0,
                path: path.clone(),
                source_url: format!("https://example.com/{}.png", chapter.id),
                mime: "image/png".into(),
                width: 1,
                height: 1,
                data: one_by_one_png(),
            };
            let html = pages_html(&source, &chapter.name, &[path]);
            db.store_chapter_content(chapter.id, &html, &[image], crate::db::RegenerateFlags::ChapterAndVolume).unwrap();
        }
        (db, source)
    }

    #[test]
    fn manga_chapter_uses_its_first_page_as_cover_when_none_is_configured() {
        let (db, source) = manga_db_with_two_chapters();
        let chapters = db.get_chapters_by_volume(db.add_volume("Example Series").unwrap()).unwrap();
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext { source: &source, processor_registry: &registry };
        let epub = build_chapter_epub(&db, &chapters[0], &ctx, false).unwrap();
        // epub-builder writes EPUB 2 by default: the cover is an item plus
        // <meta name="cover">, under OEBPS/ at the path given to add_cover_image.
        // The first page is the cover at its own path, stored once.
        let page = format!("OEBPS/images/{}-0.png", chapters[0].id);
        assert_eq!(read_zip_entry(&epub.bytes, &page), Some(one_by_one_png()));
        assert_eq!(read_zip_entry(&epub.bytes, "OEBPS/cover.png"), None);
        let opf =
            String::from_utf8(read_zip_entry(&epub.bytes, "OEBPS/content.opf").unwrap()).unwrap();
        assert!(opf.contains("<meta name=\"cover\""), "{}", opf);
    }

    /// The series volume is flagged by the fixture's stores; it is still
    /// never built, and its flag is cleared rather than left to pile up.
    #[test]
    fn manga_source_builds_chapter_epubs_only() {
        let (db, source) = manga_db_with_two_chapters();
        assert_eq!(db.get_volumes_to_regenerate().unwrap().len(), 1, "fixture flags the series volume");

        let mut config = Config::default();
        config.epub_gen = EpubGenConfig { volumes: true, chapters: true, strip_colour: true };
        config.mail.destinations.clear();
        let build = tempfile::tempdir().unwrap();
        build_epubs_for_source(&db, build.path(), &config, &source, &ProcessorRegistry::new())
            .unwrap();

        let root = build.path().join(&source.id);
        assert!(!root.join("volumes").exists());
        assert!(!root.join("volumes_stripped_colour").exists());
        assert_eq!(std::fs::read_dir(root.join("chapters/individual")).unwrap().count(), 2);
        assert!(!root.join("chapters/combined").exists());
        assert!(!root.join("chapters_stripped_colour").exists());
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }

    #[tokio::test]
    async fn first_generation_clears_flags_so_a_destination_added_later_gets_no_backlog() {
        let (db, source) = manga_db_with_two_chapters();
        let mut config = Config::default();
        config.mail.destinations.clear();
        let build = tempfile::tempdir().unwrap();
        generate_epubs_for_source(&db, build.path(), &config, &source, &ProcessorRegistry::new())
            .await
            .unwrap();
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }

    #[test]
    fn no_attachments_are_kept_when_no_destination_receives_the_source() {
        let (db, source) = manga_db_with_two_chapters();
        let mut config = Config::default();
        config.epub_gen = EpubGenConfig { volumes: true, chapters: true, strip_colour: true };
        config.mail.destinations.clear();
        let build = tempfile::tempdir().unwrap();
        let built =
            build_epubs_for_source(&db, build.path(), &config, &source, &ProcessorRegistry::new())
                .unwrap();
        let root = build.path().join(&source.id);
        assert_eq!(std::fs::read_dir(root.join("chapters/individual")).unwrap().count(), 2);
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
        assert!(built.vols.is_empty() && built.vols_stripped.is_empty());
        assert!(built.chaps.is_empty() && built.chaps_stripped.is_empty());
    }

    #[test]
    fn attachments_are_kept_when_a_destination_receives_the_source() {
        let (db, source) = manga_db_with_two_chapters();
        let mut config = Config::default();
        config.epub_gen = EpubGenConfig { volumes: true, chapters: true, strip_colour: true };
        let mut dest = crate::config::UserConfig::default();
        dest.sources.insert(source.id.clone(), Default::default());
        config.mail.destinations = vec![dest];
        let build = tempfile::tempdir().unwrap();
        // The returning variant only: it never sends.
        let built =
            build_epubs_for_source(&db, build.path(), &config, &source, &ProcessorRegistry::new())
                .unwrap();
        assert_eq!(built.chaps.len(), 2);
        assert!(built.vols.is_empty() && built.vols_stripped.is_empty(), "a manga volume is never an attachment");
        assert!(db.get_chapters_to_regenerate().unwrap().is_empty());
    }

    /// The manga skip must not reach text sources: their flagged volumes
    /// still build, in both variants.
    #[test]
    fn text_source_still_builds_its_volumes() {
        let (db, _volume, _chapters) = fixture_db();
        let source = SourceConfig { id: "test-source".into(), ..Default::default() };
        assert_eq!(db.get_volumes_to_regenerate().unwrap().len(), 1, "fixture flags the volume");
        let mut config = Config::default();
        config.epub_gen = EpubGenConfig { volumes: true, chapters: true, strip_colour: true };
        config.mail.destinations.clear();
        let build = tempfile::tempdir().unwrap();

        build_epubs_for_source(&db, build.path(), &config, &source, &ProcessorRegistry::new()).unwrap();

        let root = build.path().join(&source.id);
        assert!(root.join("volumes/Volume 1.epub").exists());
        assert!(root.join("volumes_stripped_colour/Volume 1.epub").exists());
        assert!(db.get_volumes_to_regenerate().unwrap().is_empty());
    }
}
