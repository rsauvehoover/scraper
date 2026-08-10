use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use image::io::Reader as ImageReader;
use image::{DynamicImage, Rgba, RgbaImage};
use imageproc::drawing::draw_text_mut;
use rusttype::{Font, Scale};
use std::{
    io::{Cursor, Read, Write},
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
    let font_data = std::fs::read("src/font/RobotoSlab-VariableFont_wght.ttf").ok()?;
    Font::try_from_vec(font_data)
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

fn load_stylesheet() -> String {
    let mut file = std::fs::File::open("src/assets/style.css").unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    contents
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

fn generate_chapter(
    db: &SourceDatabase,
    chapter: &Chapter,
    output_dir: &Path,
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Attachment, Box<dyn std::error::Error>> {
    let mut output = Vec::<u8>::new();
    std::fs::create_dir_all(output_dir.join("individual"))?;

    let mut epub = EpubBuilder::new(ZipLibrary::new()?)?;
    epub.metadata("author", &ctx.source.metadata.author)?;
    epub.metadata("lang", "en")?;
    epub.metadata("title", &chapter.name)?;
    epub.metadata("generator", "rsauvehoover/wandering-inn-scraper")?;

    if let Some(img_bytes) = generate_cover_with_text(ctx.source, &ctx.source.name, &chapter.name) {
        epub.add_cover_image(
            format!("{}({}).png", chapter.id, chapter.name),
            img_bytes.as_slice(),
            "image/png",
        )?;
    }
    epub.stylesheet(load_stylesheet().as_bytes())?;

    let raw_data = db.get_chapter_data(chapter.id)?;
    let processed_data = process_chapter_data(&raw_data, ctx, strip_colour);

    epub.add_content(
        EpubContent::new(
            format!("{}({}).xhtml", &chapter.id, &chapter.name),
            processed_data.as_bytes(),
        )
        .title(&chapter.name),
    )?;

    epub.generate(&mut output)?;

    let filename = format!("{}({}).epub", &chapter.id, &chapter.name);

    let mut file = std::fs::File::create(output_dir.join("individual").join(&filename))?;
    file.write_all(&output)?;
    Ok(Attachment {
        filename,
        mime: String::from("application/epub+zip"),
        bytes: output,
    })
}

fn generate_chapters(
    db: &SourceDatabase,
    chapters: &[Chapter],
    output_dir: &Path,
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Vec<Attachment>, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(output_dir.join("combined"))?;

    if chapters.is_empty() {
        return Ok(Vec::<Attachment>::default());
    }

    let mut combined_output = Vec::<u8>::new();
    let last_chapter = chapters.last().unwrap();
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
                chapters[0].id, chapters[0].name, last_chapter.id, last_chapter.name
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

        combined_epub.add_content(
            EpubContent::new(
                format!("{}({}).xhtml", chapter.id, chapter.name),
                processed_data.as_bytes(),
            )
            .title(&chapter.name),
        )?;
        attachments.push(generate_chapter(
            db,
            chapter,
            output_dir,
            ctx,
            strip_colour,
        )?);
        db.update_generated_chapter(chapter.id, false)?;
    }

    combined_epub.generate(&mut combined_output)?;

    let mut file = std::fs::File::create(output_dir.join("combined").join(format!(
        "{}({})-{}({}).epub",
        chapters[0].id, chapters[0].name, last_chapter.id, last_chapter.name
    )))?;
    file.write_all(&combined_output)?;
    Ok(attachments)
}

fn generate_volume(
    db: &SourceDatabase,
    volume: &Volume,
    chapters: &[Chapter],
    output_dir: &Path,
    ctx: &EpubContext,
    strip_colour: bool,
) -> Result<Attachment, Box<dyn std::error::Error>> {
    let mut output = Vec::<u8>::new();

    let mut epub = EpubBuilder::new(ZipLibrary::new()?)?;
    epub.metadata("author", &ctx.source.metadata.author)?;
    epub.metadata("lang", "en")?;
    epub.metadata("title", format!("{} {}", ctx.source.name, &volume.name))?;
    epub.metadata("generator", "rsauvehoover/wandering-inn-scraper")?;
    epub.stylesheet(load_stylesheet().as_bytes())?;

    if let Some(img_bytes) = generate_cover_with_text(ctx.source, &ctx.source.name, &volume.name) {
        epub.add_cover_image(
            format!("{}.png", &volume.name),
            img_bytes.as_slice(),
            "image/png",
        )?;
    }

    epub.inline_toc();

    let last_chapter_id = chapters.last().ok_or("No chapters found")?.id;
    for chapter in chapters {
        let raw_data = match db.get_chapter_data(chapter.id) {
            Err(rusqlite::Error::QueryReturnedNoRows) if chapter.id == last_chapter_id => {
                println!("  Failed to fetch data for last chapter ({}), assuming unreleased content.", chapter.name);
                continue
            },
            Err(e) => return Err(e.into()),
            Ok(data) => data,
        };

        let processed_data = process_chapter_data(&raw_data, ctx, strip_colour);

        epub.add_content(
            EpubContent::new(
                format!("{}({}).xhtml", chapter.id, chapter.name),
                processed_data.as_bytes(),
            )
            .title(&chapter.name),
        )?;
    }

    epub.generate(&mut output)?;

    std::fs::create_dir_all(output_dir)?;

    let filename = format!("{}.epub", volume.name);

    let mut file = std::fs::File::create(output_dir.join(format!("{}.epub", volume.name)))?;
    file.write_all(&output)?;

    Ok(Attachment {
        filename,
        mime: String::from("application/epub+zip"),
        bytes: output,
    })
}

/// Generate EPUBs for a specific source
pub async fn generate_epubs_for_source(
    db: &SourceDatabase,
    build_dir: &Path,
    config: &Config,
    source: &SourceConfig,
    processor_registry: &ProcessorRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
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

    if config.epub_gen.volumes {
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
                vols_stripped.push(generate_volume(
                    db,
                    &volume,
                    &chapters,
                    &source_dir.join("volumes_stripped_colour"),
                    &ctx,
                    true,
                )?);
            }
            vols.push(generate_volume(
                db,
                &volume,
                &chapters,
                &source_dir.join("volumes"),
                &ctx,
                false,
            )?);
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
            if config.epub_gen.strip_colour {
                chaps_stripped = generate_chapters(
                    db,
                    &chapters,
                    &source_dir.join("chapters_stripped_colour"),
                    &ctx,
                    true,
                )?;
            }
            chaps = generate_chapters(db, &chapters, &source_dir.join("chapters"), &ctx, false)?;
        }
    } else {
        println!("({}) Skipping chapter generation", source.id);
    }

    send_epubs(&config.mail, &source.id, &vols, &vols_stripped, &chaps, &chaps_stripped).await;

    Ok(())
}
