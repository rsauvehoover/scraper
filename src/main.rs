use std::path::Path;

use clap::Parser;

use wandering_inn_scraper::config;
use wandering_inn_scraper::db::{migration, SourceDatabase};
use wandering_inn_scraper::epub;
use wandering_inn_scraper::postprocess::ProcessorRegistry;
use wandering_inn_scraper::sources::{ScraperClient, ScraperRegistry};
use wandering_inn_scraper::web::app::{serve, WebArgs};

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

    /// Re-fetch every stored chapter of --source, recovering images, without
    /// flagging anything for EPUB generation or sending
    #[arg(long, requires = "source", conflicts_with_all = ["pull_chapter", "skip_download"])]
    redownload: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Serve the web frontend
    Web(WebArgs),
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if let Some(Command::Web(web_args)) = args.command {
        if let Err(e) = serve(web_args).await {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }

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

    // Process each source. A source that fails its index or download step is
    // skipped and its failing streak recorded in its database. The run exits
    // non-zero only for a source that has been failing for longer than
    // FAILURE_GRACE, so an hour of Cloudflare challenges does not page anyone
    // but a site that has been down all day does.
    let mut overdue = 0usize;
    let now = std::time::SystemTime::now();
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

        // Re-download only: no index update, download, EPUB generation or send
        if args.redownload {
            if let Err(e) = client
                .redownload_all(&scraper, &db, config.request_delay, &config.images)
                .await
            {
                println!("Error re-downloading chapters for {}: {}", source.id, e);
                overdue += record_failure(&db, &source.id, now);
            }
            continue;
        }

        // Update index
        if !args.skip_index {
            if let Err(e) = client.update_index(&scraper, &db, &source.ignored_volumes, &source.ignored_chapters).await {
                println!("Error updating index for {}: {}", source.id, e);
                overdue += record_failure(&db, &source.id, now);
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
                .download_all_chapters(&scraper, &db, config.request_delay, &config.images)
                .await
            {
                println!("Error downloading chapters for {}: {}", source.id, e);
                overdue += record_failure(&db, &source.id, now);
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

        if let Err(e) = db.note_success() {
            println!("({}) could not clear the failure record: {}", source.id, e);
        }
        println!("({}) Done", source.id);
    }

    println!("\n=== All sources processed ===");
    if overdue > 0 {
        println!("{} source(s) failing for over {} hours; see above", overdue, FAILURE_GRACE.as_secs() / 3600);
        std::process::exit(1);
    }
}

/// How long a source may keep failing before a run reports it: a day of
/// hourly runs. Cloudflare challenges and site hiccups clear well inside
/// that; a real outage shows up the next morning.
const FAILURE_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Record this run's failure for a source and say whether it is overdue:
/// 1 when the streak is older than FAILURE_GRACE (or the record itself
/// cannot be kept, which must not hide a failure), else 0.
fn record_failure(db: &SourceDatabase, source_id: &str, now: std::time::SystemTime) -> usize {
    let since = match db.note_failure(now) {
        Ok(since) => since,
        Err(e) => {
            println!("({}) could not record the failure: {}", source_id, e);
            return 1;
        }
    };
    let for_how_long = now.duration_since(since).unwrap_or_default();
    let hours = for_how_long.as_secs() / 3600;
    if for_how_long >= FAILURE_GRACE {
        println!("({}) failing for {} hours, past the {} hour grace", source_id, hours, FAILURE_GRACE.as_secs() / 3600);
        1
    } else {
        println!("({}) failing for {} hours; the run stays green until {}", source_id, hours, FAILURE_GRACE.as_secs() / 3600);
        0
    }
}
