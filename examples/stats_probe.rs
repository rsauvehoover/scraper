use wandering_inn_scraper::config::load_config;
use wandering_inn_scraper::db::SourceRegistry;
use wandering_inn_scraper::stats::cache::StatsCache;

fn main() {
    let config = load_config();
    let registry = SourceRegistry::from_config(&config);
    let cache = StatsCache::new();

    let mut total_words = 0usize;
    let mut total_chapters = 0usize;
    for entry in registry.entries() {
        let stat = cache.get(entry);
        println!(
            "{:<28} {:>6} chapters {:>14} words",
            stat.name, stat.total_chapters, stat.total_words
        );
        total_words += stat.total_words;
        total_chapters += stat.total_chapters;
    }
    println!("{:<28} {:>6} chapters {:>14} words", "ALL", total_chapters, total_words);
}
