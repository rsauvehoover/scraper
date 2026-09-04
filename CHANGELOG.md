# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.2.0](https://github.com/rsauvehoover/scraper/releases/tag/v2.2.0) - 2026-09-04

### Added

- per-source wordcount script ([#16](https://github.com/rsauvehoover/scraper/pull/16))
- --pull-chapter flag for pre-TOC chapters ([#15](https://github.com/rsauvehoover/scraper/pull/15))
- added patreon chapter parsing first draft
- hacked together good enough mail sending for now
- updated config to provide more epub generation options
- add action concurrency
- added build actions
- added bundling and fixed some small bugs
- added individual chapter epub generation
- added colour stripping
- added cover generation and reorganized directories
- added config defaults and improved regex replace
- added chapter data download
- toc is now indexed and stored in db
- initial commit and add config parsing skeleton

### Fixed

- sanitize path separators in epub filenames ([#21](https://github.com/rsauvehoover/scraper/pull/21))
- fix late init warning
- stripped colour on volumes was reversed
- fixed dead code warning on db ids
- added user_agent header to chapter download call
- fixed warning for unhandled result
- fixed a minor typo
- temporary fix to not download patreon chapters
- forgot closing html tag
- double escape certain characters since beautifulsoup is weird
- parse mrsha speak and colours correctly
- better release profile to fix build issue
- fixed latest tag grabbing
- add cover for individual chapters

### Other

- automate releases with release-plz ([#24](https://github.com/rsauvehoover/scraper/pull/24))
- Version bump ([#23](https://github.com/rsauvehoover/scraper/pull/23))
- commit Cargo.lock, update mail-builder to 0.5 ([#22](https://github.com/rsauvehoover/scraper/pull/22))
- Ignore content errors in volume generation if it's the last chapter ([#20](https://github.com/rsauvehoover/scraper/pull/20))
- Fix some compiler warnings, upgrade some deps ([#19](https://github.com/rsauvehoover/scraper/pull/19))
- Add ability to ignore volumes or chapters ([#18](https://github.com/rsauvehoover/scraper/pull/18))
- rename ([#14](https://github.com/rsauvehoover/scraper/pull/14))
- Fix/royal road ([#13](https://github.com/rsauvehoover/scraper/pull/13))
- Feat/per source overrides ([#12](https://github.com/rsauvehoover/scraper/pull/12))
- Workflow fixes ([#11](https://github.com/rsauvehoover/scraper/pull/11))
- 2.0.0 ([#10](https://github.com/rsauvehoover/scraper/pull/10))
- Feat/other sources refactor ([#9](https://github.com/rsauvehoover/scraper/pull/9))
- remove old config
- remove some unecessary cookies
- Use patreon integration instead
- fixed some clippy hints
- 1.2.0
- updated .gitignore to ignore .DS_Store
- 1.1.4
- 1.1.3
- formatting
- 1.1.2
- Merge branch 'main' of github.com:rsauvehoover/wandering_inn_scraper
- 1.1.1
- updated readme
- 1.1.0
- updated wix build
- update action to actually be disabled for now
- disable build for now
- 0.2.0
- formatting
- added soup first tests, and split out submodules
- Initial commit
