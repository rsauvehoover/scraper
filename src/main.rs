use std::path::Path;

use clap::Parser;

mod config;
mod db;
mod epub;
mod error;
mod mail;
mod postprocess;
mod sources;

use db::{migration, SourceDatabase};
use postprocess::ProcessorRegistry;
use sources::{ScraperClient, ScraperRegistry};

/// Multi-source web serial scraper
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Process only the specified source ID
    #[arg(short, long)]
    source: Option<String>,

    /// Skip downloading new chapters
    #[arg(long)]
    skip_download: bool,

    /// Skip EPUB generation
    #[arg(long)]
    skip_epub: bool,

    /// Skip index update
    #[arg(long)]
    skip_index: bool,

    /// Pull a chapter URL not yet on the TOC (requires --source)
    #[arg(long, requires = "source", value_name = "URL")]
    pull_chapter: Option<String>,

    /// Volume name for --pull-chapter (default: latest volume in the DB)
    #[arg(long, requires = "pull_chapter", value_name = "NAME")]
    volume: Option<String>,

    /// Chapter title for --pull-chapter (default: parsed from the page)
    #[arg(long, requires = "pull_chapter", value_name = "NAME")]
    title: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = config::load_config();

    // Migrate legacy database if needed
    if migration::needs_migration() {
        match migration::migrate_legacy_database() {
            Ok(migrated) => {
                if migrated {
                    println!("Legacy database migrated successfully");
                }
            }
            Err(e) => {
                panic!("Error migrating legacy database: {}", e);
            }
        }
    }

    // Initialize registries
    let scraper_registry = ScraperRegistry::from_config(&config);
    let processor_registry = ProcessorRegistry::new();
    let client = ScraperClient::new().expect("Failed to create HTTP client");

    // Determine which sources to process
    let sources_to_process: Vec<_> = if let Some(ref source_id) = args.source {
        config
            .enabled_sources()
            .filter(|s| s.id == *source_id)
            .collect()
    } else {
        config.enabled_sources().collect()
    };

    if sources_to_process.is_empty() {
        if let Some(ref source_id) = args.source {
            panic!("Source '{}' not found or not enabled", source_id);
        } else {
            println!("No enabled sources found");
            return;
        }
    }

    // Process each source
    for source in sources_to_process {
        println!(
            "\n=== Processing source: {} ({}) ===",
            source.name, source.id
        );

        // Open database for this source
        let db = match SourceDatabase::open(&source.id) {
            Ok(db) => db,
            Err(e) => {
                println!("Error opening database for {}: {}", source.id, e);
                continue;
            }
        };

        // Get scraper for this source
        let scraper = match scraper_registry.get(&source.id) {
            Ok(s) => s,
            Err(e) => {
                println!("Error getting scraper for {}: {}", source.id, e);
                continue;
            }
        };

        // Update index
        if !args.skip_index {
            if let Err(e) = client.update_index(&scraper, &db).await {
                println!("Error updating index for {}: {}", source.id, e);
                continue;
            }
        } else {
            println!("({}) Skipping index update", source.id);
        }

        // Seed a manually pulled chapter (--pull-chapter requires --source,
        // so this loop only runs for that source)
        if let Some(ref url) = args.pull_chapter {
            if let Err(e) = client
                .seed_chapter(
                    &scraper,
                    &db,
                    url,
                    args.title.as_deref(),
                    args.volume.as_deref(),
                )
                .await
            {
                println!("Error pulling chapter for {}: {}", source.id, e);
                continue;
            }
        }

        // Download chapters
        if !args.skip_download {
            if let Err(e) = client
                .download_all_chapters(&scraper, &db, config.request_delay)
                .await
            {
                println!("Error downloading chapters for {}: {}", source.id, e);
                continue;
            }
        } else {
            println!("({}) Skipping chapter download", source.id);
        }

        // Generate EPUBs
        if !args.skip_epub {
            if let Err(e) = epub::generate_epubs_for_source(
                &db,
                Path::new("build/"),
                &config,
                source,
                &processor_registry,
            )
            .await
            {
                println!("Error generating EPUBs for {}: {}", source.id, e);
                continue;
            }
        } else {
            println!("({}) Skipping EPUB generation", source.id);
        }

        println!("({}) Done", source.id);
    }

    println!("\n=== All sources processed ===");
}
