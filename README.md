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

## Web frontend

`wandering_inn_scraper web` serves a read-only view of the scraped data plus a
configuration editor. It runs alongside the scraper and never writes to the
databases through SQLite.

| Flag | Default | Description |
|------|---------|-------------|
| `--bind` | `127.0.0.1:8080` | Address to listen on |
| `--auth-file` | `web-auth.json` | Admin credential (argon2id), mode 600 |
| `--config-file` | `config.json` | The configuration this service edits |
| `--secure-cookies` | off | Set the Secure flag on the session cookie; turn on when TLS reaches this service directly |
| `--trust-forwarded-for` | off | Trust `X-Forwarded-For` for login rate limiting; only enable behind a reverse proxy that overwrites the header |
| `--set-password` | | Prompt for a new admin password, write it, and exit |

The server is a subcommand of the scraper binary, not a separate one:
`wandering_inn_scraper web`. The packaging step produces one binary per
invocation, so a second binary would mean a second package to build, ship,
and version — the subcommand keeps it to one.

Set a password before the first run; the server refuses to start without one:

```bash
wandering_inn_scraper web --set-password
```

The admin credential is deliberately kept out of `config.json`, because this
service can rewrite `config.json`.

Run it from the same working directory as the scraper: `config.json`, `db/`
and `build/` are all resolved relative to the process's current directory,
not the binary's location.

It opens the databases with `PRAGMA query_only`, so it never writes through
SQLite. It still needs filesystem **write** permission on `db/` — that is not
a mistake: SQLite's WAL mode requires even a read-only connection to be able
to create and update the `-shm` shared-memory index file, so a reader that
cannot write to the directory cannot open the database at all.

It never creates a database, though, so a source that is configured but has
never been scraped has no `db/{source-id}.db` for it to read. Such a source is
skipped at startup with a warning on stderr and does not appear in the UI;
run the scraper for it once and restart. The same applies to a database that
exists but has no tables. One unreadable source never stops the server from
starting.

The admin password is read once at process startup and held in memory for
the life of the process. Rotating it with `--set-password` writes the new
credential to `--auth-file` immediately, but the running server keeps using
the old one until it is restarted. If the old password still works after a
rotation, that means "restart pending", not "rotation failed".

The configuration editor writes `config.json` immediately, and the next
scraper run reads the new file, but this server reads the source list once at
startup. Adding, removing or renaming a source therefore does not change what
the UI shows until the web service is restarted — the save confirmation says
so. Mail and EPUB settings have no such caveat: only the scraper reads those,
and it reads them per run.

The server and the scraper are one binary, so a binary upgrade replaces the
file on disk without restarting whatever process is already running it.
After upgrading, restart the web service explicitly and check the version in
the page footer rather than trusting the installed package version.

### Running as a service

The server reads the admin credential at startup and refuses to serve without
one. Set it before enabling a service unit:

```bash
wandering_inn_scraper web --set-password
```

Enabling the unit first is not harmful, but the service exits immediately with
an error naming the missing file, and a unit with `Restart=on-failure` repeats
that every few seconds until the credential exists.

`config.json`, `db/` and the credential file are all resolved relative to the
process working directory, so a unit must set its working directory to the
scraper's data directory. A service started anywhere else starts cleanly and
reports every configured source as configured but not yet scraped — that is
the signature of a wrong working directory, not of missing databases.

## Build

Binaries will be found `target/release/bundle` and `target/wix` directories

### Linux/MacOS
```bash
cargo bundle --release --format deb
```

On Linux, a bare `cargo bundle --release` also attempts an AppImage, which
needs `mksquashfs` from `squashfs-tools`. Without it the command exits 1
*after* it has already written a complete `.deb`, so a script or CI job that
trusts the exit code throws away a good package. `--format deb` avoids that;
installing `squashfs-tools` also works.

The `.deb` filename is built from the crate name while the package inside is
named by `[package.metadata.bundle] name`, so the two disagree:
`wandering_inn_scraper_<version>_<arch>.deb` contains the package `scraper`.
Confirm with `dpkg-deb -f <file> Package`. Renaming the package also means a
new install does not upgrade an older one in place — both ship the same
binary path, so remove the old package before installing the new one.

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
      "IgnoredVolumes": ["Ignored Volume Title"],
      "IgnoredChapters": ["Ignored Chapter Title"],

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
