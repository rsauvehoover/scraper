use wandering_inn_scraper::config::load_config;
use wandering_inn_scraper::db::{SkipReason, SourceRegistry};
use wandering_inn_scraper::stats::cache::StatsCache;

fn main() {
    let config = load_config();
    let registry = SourceRegistry::from_config(&config);
    let cache = StatsCache::new();

    let mut total_words = 0usize;
    let mut total_chapters = 0usize;
    let mut total_pending = 0usize;
    for entry in registry.entries() {
        let stat = match cache.get(entry) {
            Ok(stat) => stat,
            Err(e) => {
                eprintln!("{:<28} unavailable: {}", entry.config.name, e);
                continue;
            }
        };
        println!(
            "{:<28} {:>6} chapters {:>14} words {:>6} pending",
            stat.name, stat.total_chapters, stat.total_words, stat.pending_chapters
        );
        total_words += stat.total_words;
        total_chapters += stat.total_chapters;
        total_pending += stat.pending_chapters;
    }
    println!(
        "{:<28} {:>6} chapters {:>14} words {:>6} pending",
        "ALL", total_chapters, total_words, total_pending
    );

    // `registry.entries()` above already excludes these, so without this the
    // ALL line quietly under-counts every source that was configured but not
    // admitted to the registry, with nothing here to say so.
    let skipped = registry.skipped();
    if !skipped.is_empty() {
        println!("{} source(s) excluded from ALL:", skipped.len());
        for source in skipped {
            let reason = match source.reason {
                SkipReason::NotYetScraped => "not yet scraped",
                SkipReason::Broken => "broken, see warnings above",
            };
            println!("{:<28} skipped: {}", source.name, reason);
        }
    }
}
