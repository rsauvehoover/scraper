use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use image::io::Reader as ImageReader;
use image::Rgba;
use imageproc::drawing::draw_text_mut;
use rusttype::{Font, Scale};
use std::{
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
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

fn generate_cover(
    volume_title: &str,
    output_dir: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut img = ImageReader::open("src/assets/cover.png")
        .unwrap()
        .decode()
        .unwrap();

    let font = Vec::from(include_bytes!("font/RobotoSlab-VariableFont_wght.ttf") as &[u8]);
    let font = Font::try_from_vec(font).unwrap();

    draw_text_mut(
        &mut img,
        Rgba([255, 255, 60, 255]),
        15,
        112,
        Scale::uniform(30.0),
        &font,
        volume_title,
    );
    std::fs::create_dir_all(output_dir)?;
    let path = output_dir.join(format!("{}.png", &volume_title));
    img.save(&path)?;
    Ok(path)
}

fn load_stylesheet() -> String {
    let mut file = std::fs::File::open("src/assets/style.css").unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    contents
}

fn process_chapter_data(
    raw_data: &str,
    ctx: &EpubContext,
    strip_colour: bool,
) -> String {
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

    let cover_img = generate_cover(
        &format!("Chapter {}", chapter.name),
        &output_dir.join("..").join("covers"),
    );
    let img_file = ImageReader::open(cover_img?)?.decode()?;
    let mut img_bytes = Vec::new();
    img_file.write_to(
        &mut Cursor::new(&mut img_bytes),
        image::ImageOutputFormat::Png,
    )?;
    epub.add_cover_image(
        output_dir.join(format!("{}({}).png", chapter.id, chapter.name)),
        img_bytes.as_slice(),
        "image/png",
    )?;
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

    let cover_img = generate_cover(
        &format!("Chapters {}-{}", chapters[0].name, last_chapter.name),
        &output_dir.join("..").join("covers"),
    );
    let img_file = ImageReader::open(cover_img?)?.decode()?;
    let mut img_bytes = Vec::new();
    img_file.write_to(
        &mut Cursor::new(&mut img_bytes),
        image::ImageOutputFormat::Png,
    )?;
    combined_epub.add_cover_image(
        output_dir.join(format!(
            "{}({})-{}({}).png",
            chapters[0].id, chapters[0].name, last_chapter.id, last_chapter.name
        )),
        img_bytes.as_slice(),
        "image/png",
    )?;
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
        attachments.push(generate_chapter(db, chapter, output_dir, ctx, strip_colour)?);
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

    let cover_img = generate_cover(&volume.name, &output_dir.join("..").join("covers"));
    let img_file = ImageReader::open(cover_img?)?.decode()?;
    let mut img_bytes = Vec::new();
    img_file.write_to(
        &mut Cursor::new(&mut img_bytes),
        image::ImageOutputFormat::Png,
    )?;
    epub.add_cover_image(
        output_dir.join(format!("{}.png", &volume.name)),
        img_bytes.as_slice(),
        "image/png",
    )?;

    epub.inline_toc();

    for chapter in chapters {
        let raw_data = db.get_chapter_data(chapter.id)?;
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
            println!("({}) Generating epubs for {} volumes", source.id, volumes.len());
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
            println!("({}) Generating epubs for {} chapters", source.id, chapters.len());
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

    send_epubs(&config.mail, &vols, &vols_stripped, &chaps, &chaps_stripped).await;

    Ok(())
}
