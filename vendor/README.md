# Vendored crates

## epub-builder

Copy of [epub-builder 0.8.3](https://crates.io/crates/epub-builder/0.8.3)
(MPL-2.0, licence in `epub-builder/LICENSE`). The crates.io release pins
`zip = "6.0.0"`, which is affected by GHSA-m665-v7pm-7grj, and its `Zip`
trait is private, so the only way to build EPUBs on a patched `zip` is to
carry the crate.

Changes from the release:

- `Cargo.toml`: `libzip` (the `zip` crate) raised from `6.0.0` to `9.0.5`;
  packaging files, examples and CI config dropped.
- `src/zip_library.rs`: `ZipWriter::set_comment` returns a `Result` in zip 9;
  it is propagated instead of ignored.

Drop this directory and go back to the crates.io dependency once a release
depends on `zip >= 9.0.5`.
