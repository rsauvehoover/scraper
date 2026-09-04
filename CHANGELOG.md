# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.2.0](https://github.com/rsauvehoover/scraper/releases/tag/v2.2.0) - 2026-09-04

### Added

- per-source wordcount script ([#16](https://github.com/rsauvehoover/scraper/pull/16))
- --pull-chapter flag for pre-TOC chapters ([#15](https://github.com/rsauvehoover/scraper/pull/15))
- ability to ignore volumes or chapters ([#18](https://github.com/rsauvehoover/scraper/pull/18))

### Fixed

- sanitize path separators in epub filenames ([#21](https://github.com/rsauvehoover/scraper/pull/21))
- ignore content errors in volume generation if it's the last chapter ([#20](https://github.com/rsauvehoover/scraper/pull/20))
- Royal Road scraper fixes ([#13](https://github.com/rsauvehoover/scraper/pull/13))

### Other

- automate releases with release-plz ([#24](https://github.com/rsauvehoover/scraper/pull/24))
- commit Cargo.lock, update mail-builder to 0.5 ([#22](https://github.com/rsauvehoover/scraper/pull/22))
- fix some compiler warnings, upgrade some deps ([#19](https://github.com/rsauvehoover/scraper/pull/19))
- rename repository ([#14](https://github.com/rsauvehoover/scraper/pull/14))
