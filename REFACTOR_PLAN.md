# Multi-Source Web Serial Scraper Refactor

## Overview

Refactor the Wandering Inn scraper to support multiple web serials with:
1. **Configurable selectors** per source (content divs, chapter entries, etc.)
2. **Separate SQLite databases** per source
3. **Plugin-like parsing/processing modules** via traits

## Architecture Summary

```
src/
  main.rs                 # Orchestration loop over sources
  config.rs               # Multi-source config + backward compat
  error.rs                # Custom error types (new)

  sources/                # NEW module
    mod.rs
    traits.rs             # SourceScraper trait
    generic.rs            # Config-driven GenericScraper
    wandering_inn.rs      # Custom TWI scraper (wraps generic)
    registry.rs           # ScraperRegistry

  db/                     # Refactored module
    mod.rs
    connection.rs         # SourceDatabase struct
    manager.rs            # DatabaseManager (multi-DB)
    migration.rs          # Legacy DB migration
    models.rs             # Chapter, Volume structs

  postprocess/            # NEW module
    mod.rs                # ProcessorRegistry + trait
    strip_links.rs
    strip_colour.rs
    mrsha_write.rs

  epub/                   # Refactored module
    mod.rs
    generator.rs          # Uses source metadata
    cover.rs
```

## Configuration Schema

```jsonc
{
  "RequestDelay": 1000,
  "Mail": { /* unchanged */ },
  "EpubGen": { /* global defaults */ },

  "Sources": [
    {
      "Id": "wandering-inn",
      "Name": "The Wandering Inn",
      "Enabled": true,
      "TocUrl": "https://wanderinginn.com/table-of-contents/",

      "Selectors": {
        "VolumeWrapper": "volume-wrapper",
        "VolumeTitle": "h2",
        "ChapterEntry": "chapter-entry",
        "ChapterLink": "a",
        "MainContent": "main-content",
        "SelectorType": "class"  // "class" | "id" | "tag"
      },

      "Auth": {
        "Type": "patreon",
        "PatreonName": "username"
      },

      "Metadata": {
        "Author": "pirate aba",
        "Description": "The Wandering Inn"
      },

      "PostProcessors": ["mrsha-write", "strip-links"]
    }
  ]
}
```

Backward compatibility: If `Sources` is empty but `TocUrl` exists, auto-generate a Wandering Inn source config.

---

## Core Traits

### SourceScraper (src/sources/traits.rs)

```rust
#[async_trait]
pub trait SourceScraper: Send + Sync {
    fn source_id(&self) -> &str;
    fn source_name(&self) -> &str;
    async fn parse_toc(&self, html: &str) -> Result<Vec<ScrapedChapter>, ScrapeError>;
    async fn parse_chapter(&self, html: &str, title: &str) -> Result<ChapterContent, ScrapeError>;
    fn build_auth_headers(&self) -> Option<Vec<(String, String)>>;
}
```

### PostProcessor (src/postprocess/mod.rs)

```rust
pub trait PostProcessor: Send + Sync {
    fn name(&self) -> &str;
    fn process(&self, content: &str) -> String;
}
```

---

## Database Strategy

Each source gets its own SQLite file:
```
db/
  wandering-inn.db
  practical-guide.db
```

Migration: Rename existing `db/index.db` to `db/wandering-inn.db` and add source metadata table.

---

## Implementation Phases

### Phase 1: Config Refactor
**Files:** `src/config.rs`

1. Add new types: `SourceConfig`, `Selectors`, `AuthConfig`, `SourceMetadata`
2. Update `Config` to have `sources: Vec<SourceConfig>`
3. Implement backward-compatibility migration in `load_config()`

### Phase 2: Database Multi-Source
**Files:** `src/db.rs` → `src/db/`

1. Create `SourceDatabase` struct with source-specific path
2. Create `DatabaseManager` to track open databases
3. Add `migrate_legacy_database()` function
4. Move query helpers to instance methods

### Phase 3: Source Scraper Traits
**Files:** `src/scraper.rs` → `src/sources/`

1. Define `SourceScraper` trait in `traits.rs`
2. Implement `GenericScraper` that reads selectors from config
3. Implement `WanderingInnScraper` wrapping generic + Patreon detection
4. Create `ScraperRegistry` to build scrapers from config

### Phase 4: Post-Processing Pipeline
**Files:** Extract from `src/epub.rs` → `src/postprocess/`

1. Define `PostProcessor` trait
2. Extract `strip_links`, `strip_colour`, `mrsha_write` as separate processors
3. Create `ProcessorRegistry` with `apply_chain()` method
4. Update EPUB generation to use registry

### Phase 5: Main Integration
**Files:** `src/main.rs`, `src/epub.rs`

1. Initialize registries from config
2. Loop over enabled sources
3. Per-source: open DB, get scraper, update index, download, generate EPUBs
4. Update EPUB generator to use source metadata

### Phase 6: Polish
1. Add `--source <id>` CLI flag to process single source
2. Add error handling with `thiserror`
3. Update `config.json` example with multi-source
4. Update README

---

## Key Implementation Details

### GenericScraper selector usage

```rust
fn parse_toc(&self, html: &str) -> Result<Vec<ScrapedChapter>> {
    let soup = Soup::new(html);
    for volume in soup.class(&self.config.selectors.volume_wrapper).find_all() {
        let title = volume.tag(&self.config.selectors.volume_title).find();
        for chapter in volume.class(&self.config.selectors.chapter_entry).find_all() {
            let link = chapter.tag(&self.config.selectors.chapter_link).find();
            // ...
        }
    }
}
```

### Database path from source ID

```rust
impl SourceDatabase {
    pub fn open(source_id: &str) -> Result<Self> {
        let db_path = PathBuf::from("db").join(format!("{}.db", source_id));
        let conn = Connection::open(&db_path)?;
        // ...
    }
}
```

### Post-processor chain

```rust
let mut content = db.get_chapter_data(chapter_id)?;
content = processor_registry.apply_chain(&content, &source.post_processors);
```

---

## Files to Modify/Create

| File | Action | Description |
|------|--------|-------------|
| `src/config.rs` | Rewrite | Multi-source config types + backward compat |
| `src/scraper.rs` | Delete | Logic moves to `src/sources/` |
| `src/db.rs` | Refactor → `src/db/` | Split into connection.rs, manager.rs, models.rs |
| `src/epub.rs` | Refactor | Extract processors, use source metadata |
| `src/main.rs` | Rewrite | Multi-source orchestration loop |
| `src/sources/` | Create | traits.rs, generic.rs, wandering_inn.rs, registry.rs |
| `src/postprocess/` | Create | mod.rs + individual processors |
| `src/error.rs` | Create | Custom error types |

---

## Verification

1. **Backward compatibility**: Run with existing `config.json` - should auto-migrate to single-source format
2. **Multi-source**: Create test config with 2 sources, verify separate DBs created
3. **Selectors**: Configure different selectors, verify TOC parsing works
4. **Post-processors**: Verify `mrsha-write` and `strip-colour` work via config
5. **EPUBs**: Generate EPUB, verify metadata matches source config
