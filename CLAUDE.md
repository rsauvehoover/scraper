# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
cargo build --release          # Build optimized binary
cargo run --release            # Run scraper
cargo run -- --help            # Show CLI options
```

**CLI flags:**
- `--source <id>` - Process single source only
- `--skip-download` - Skip chapter downloads
- `--skip-epub` - Skip EPUB generation
- `--skip-index` - Skip TOC index updates

## Architecture

This is a multi-source web serial scraper with plugin-like extensibility via traits.

### Core Traits

**`SourceScraper`** (`src/sources/traits.rs`): Defines how to scrape a source
- `parse_toc()` - Extract chapters from table of contents
- `parse_chapter()` - Extract content from chapter page
- `build_auth_headers()` - Authentication (e.g., Patreon cookies)

**`PostProcessor`** (`src/postprocess/mod.rs`): Content transformations
- `process()` - Transform HTML content (strip links, convert colored text, etc.)

### Registry Pattern

Both traits use registries for runtime lookup:
- `ScraperRegistry::from_config()` - Builds scrapers from config, uses `GenericScraper` for unknown sources
- `ProcessorRegistry::new()` - Registers all processors, `apply_chain()` runs them in sequence

### Multi-Source Database

Each source gets its own SQLite file: `db/{source-id}.db`
- `SourceDatabase::open(source_id)` creates/opens the database
- Legacy `db/index.db` auto-migrates to `db/wandering-inn.db`

### Data Flow

```
Config → ScraperRegistry → per source:
  SourceDatabase.open() → SourceScraper.parse_toc() → download chapters
  → ProcessorRegistry.apply_chain() → EPUB generation
```

### Configuration

Sources are configured in `config.json` with:
- Selectors: `volume_wrapper`, `chapter_entry`, `main_content`, etc.
- SelectorType: `class`, `id`, or `tag`
- Auth: `None` or `Patreon { patreon_name }`
- PostProcessors: ordered list like `["mrsha-write", "strip-links"]`

Backward compatible: legacy single-source configs auto-migrate to multi-source format.

#### Per-User Source Config

Each user destination's `Sources` is a map of source ID to per-source overrides:

```json
{
  "Name": "Kindle Upload",
  "Email": "user@kindle.com",
  "StripColour": false,
  "SendFullVolumes": false,
  "SendIndividualChapters": true,
  "Sources": {
    "wandering-inn": { "StripColour": true },
    "royal-road-pale-lights": {}
  }
}
```

- Top-level `StripColour`, `SendFullVolumes`, `SendIndividualChapters` are defaults for all sources
- Per-source entries can override any of these with explicit values; omitted fields inherit the defaults
- An empty `Sources` map (or omitted) means the user receives all sources with their defaults

## Adding New Sources

1. Add source config to `config.json` with appropriate selectors
2. If generic parsing doesn't work, create custom scraper in `src/sources/` implementing `SourceScraper`
3. Register in `ScraperRegistry::create_scraper()` match statement

### Royal Road Sources

Royal Road sources are supported via the `RoyalRoadScraper`. To add a Royal Road fiction:

1. Add a source config with an ID starting with `royal-road-` (e.g., `royal-road-65058`)
2. The scraper handles:
   - Parsing the chapter table (`table#chapters`)
   - Extracting content from `div.chapter-content`
   - **Chapter discovery**: Since Royal Road paginates its TOC, the scraper discovers additional chapters by following `<link rel="next">` headers during download

Example config:
```json
{
  "Sources": [
    {
      "Id": "royal-road-65058",
      "Name": "Pale Lights",
      "Enabled": true,
      "TocUrl": "https://www.royalroad.com/fiction/65058/pale-lights",
      "Auth": { "Type": "None" },
      "Metadata": {
        "Author": "ErraticErrata",
        "Description": "From the author of A Practical Guide to Evil"
      },
      "PostProcessors": ["strip-links"]
    }
  ]
}
```

## Adding Post-Processors

1. Create processor in `src/postprocess/{category}/` implementing `PostProcessor`
2. Export in category's `mod.rs`
3. Register in `ProcessorRegistry::new()`
4. Reference by name in source's `PostProcessors` config array
