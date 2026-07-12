# scraper

## Description

A configurable web serial scraper that generates EPUBs. Supports multiple sources including custom sites and Royal Road. Originally built for [The Wandering Inn](https://wanderinginn.com/), but designed to work with any web serial that has a table of contents page.

## Usage

1. Build the latest version, or download [the latest release](https://github.com/rsauvehoover/wandering_inn_scraper/releases)
2. add a `config.json` to the same directory as the binary or the root of the project if building from source.
See [Configuration](#configuration) below for all options.
3. Run the program, outputs will be in the `build` directory.
NOTE: While you can run run the program by double clicking the binary, it will close immediately after finishing
and you won't be able to see any output. It is recommended to run from a terminal.

## Building/running locally

1. Ensure you have rust installed, if not install [here](https://www.rust-lang.org/tools/install).
2. Clone this repo.
```bash
git clone https://github.com/rsauvehoover/wandering_inn_scraper.git
```
3. Build the project with cargo. `--release` flag is optional if you don't want optimizations.
This step can be skipped if you want, `cargo run` will also build if necessary
```bash
cargo build --release
```
4. Run the project with cargo. `--release` flag is optional if you don't want optimizations
```bash
cargo run --release
```

## CLI flags

Run with no flags to process every enabled source (index update, chapter download, EPUB generation, mail).

| Flag | Description |
|------|-------------|
| `--source <id>` | Process only the specified source ID |
| `--skip-download` | Skip downloading new chapters |
| `--skip-epub` | Skip EPUB generation (also skips mail) |
| `--skip-index` | Skip TOC index updates |
| `--pull-chapter <URL>` | Seed a chapter that isn't on the TOC yet (requires `--source`) |
| `--volume <NAME>` | Volume for `--pull-chapter` (default: latest volume in the DB) |
| `--title <NAME>` | Title for `--pull-chapter` (default: parsed from the chapter page) |

### Pulling a chapter early

If a chapter is live but not yet listed on the source's table of contents, you can pull it directly:

```bash
cargo run --release -- --source my-serial --pull-chapter https://example.com/2026/07/05/chapter-10/
```

The chapter is downloaded, generated, and mailed like any other. When the TOC later lists it, the existing
entry is matched by URL and updated in place, so the chapter is never duplicated or re-sent. Pass the URL
in the same form the TOC will use (same scheme/host/path; a trailing-slash difference is tolerated).
Re-running with an already-seeded URL is a no-op.

## Build

Binaries will be found `target/release/bundle` and `target/wix` directories

### Linux/MacOS
```bash
cargo bundle --release
```

### Windows
NOTE: `cargo wix` doesn't show any output by default, run with `-v` and `--nocapture` flags to see verbose output.
```bash
cargo wix
```

## Versioning

```bash
cargo bump {major|minor|patch} --git-tag
```

## Configuration

Create a `config.json` file with the following structure:

```jsonc
{
  // Delay between HTTP requests in milliseconds (default: 1000)
  "RequestDelay": 1000,

  // Email configuration for sending EPUBs (optional)
  "Mail": {
    "Name": "Sender Name",
    "Address": "sender@example.com",
    "Password": "app-password",           // Gmail app password, not your regular password
    "SmtpHostname": "smtp.gmail.com",
    "SmtpPort": 587,
    "Destinations": [
      {
        "Name": "Recipient",
        "Email": "recipient@example.com",
        "StripColour": false,             // Default: remove colored text styling
        "SendFullVolumes": true,          // Default: send complete volume EPUBs
        "SendIndividualChapters": false,  // Default: send each chapter as separate EPUB

        // Map of source ID to per-source overrides (empty map or omitted = all sources with defaults)
        "Sources": {
          "my-serial": {},                           // Inherits all defaults above
          "another-serial": {                        // Override specific settings for this source
            "StripColour": true,
            "SendFullVolumes": false
          }
        }
      }
    ]
  },

  // EPUB generation settings
  "EpubGen": {
    "Volumes": true,      // Generate volume EPUBs
    "Chapters": true,     // Generate individual chapter EPUBs
    "StripColour": false  // Remove colored text styling globally
  },

  // Sources to scrape
  "Sources": [
    {
      "Id": "my-serial",                  // Unique identifier (used for database file)
      "Name": "My Web Serial",            // Human-readable name
      "Enabled": true,
      "TocUrl": "https://example.com/table-of-contents/",

      // HTML selectors for parsing (required for generic sources)
      "Selectors": {
        "VolumeWrapper": "volume-wrapper",  // Container for each volume
        "VolumeTitle": "h2",                // Tag for volume title
        "ChapterEntry": "chapter-entry",    // Container for each chapter link
        "ChapterLink": "a",                 // Tag for chapter link
        "MainContent": "main-content",      // Container for chapter content
        "SelectorType": "class"             // How to interpret selectors: "class", "id", or "tag"
      },

      // Authentication (optional)
      "Auth": {
        "Type": "None"  // Or "Patreon" with "PatreonName": "your-username"
      },

      // Metadata for EPUB generation
      "Metadata": {
        "Author": "Author Name",
        "Description": "A great web serial",
        "CoverImage": "covers/my-serial.jpg"  // Optional path to cover image
      },

      // Post-processors to apply in order
      // Available: "mrsha-write", "strip-links", "strip-colour"
      "PostProcessors": ["strip-links"]
    },

    // Royal Road example - uses built-in scraper, no Selectors needed
    {
      "Id": "royal-road-12345",           // Must start with "royal-road-" for auto-detection
      "Name": "My Royal Road Fiction",
      "Enabled": false,
      "TocUrl": "https://www.royalroad.com/fiction/12345/my-fiction",
      "Auth": { "Type": "None" },
      "Metadata": {
        "Author": "Author Name",
        "Description": "An exciting story",
        "CoverImage": "covers/my-fiction.jpg"
      },
      "PostProcessors": ["strip-links"]
    },

    // Patreon-authenticated source example
    {
      "Id": "my-serial-patreon",
      "Name": "My Web Serial (Patreon)",
      "Enabled": false,
      "TocUrl": "https://example.com/table-of-contents/",
      "Selectors": {
        "VolumeWrapper": "volume-wrapper",
        "VolumeTitle": "h2",
        "ChapterEntry": "chapter-entry",
        "ChapterLink": "a",
        "MainContent": "main-content",
        "SelectorType": "class"
      },
      "Auth": {
        "Type": "Patreon",
        "PatreonName": "your-patreon-username"  // Your Patreon login username
      },
      "Metadata": {
        "Author": "Author Name",
        "Description": "A great web serial"
      },
      "PostProcessors": ["strip-links"]
    }
  ]
}
```

### Minimal Configuration

A minimal setup for a single source:

```json
{
  "Sources": [
    {
      "Id": "my-serial",
      "Name": "My Web Serial",
      "Enabled": true,
      "TocUrl": "https://example.com/table-of-contents/",
      "Selectors": {
        "VolumeWrapper": "volume",
        "ChapterEntry": "chapter",
        "MainContent": "content",
        "SelectorType": "class"
      },
      "Auth": { "Type": "None" },
      "Metadata": {
        "Author": "Author Name",
        "Description": "My Web Serial"
      },
      "PostProcessors": ["strip-links"]
    }
  ]
}
```

All other fields have sensible defaults.
