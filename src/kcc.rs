//! Manga EPUBs through Kindle Comic Converter.
//!
//! KCC does what a comic reader needs and this crate does not: pages sized
//! to the device, double-page spreads split in right-to-left order, a size
//! ceiling, and an output Amazon's Send to Kindle converts well. The scraper
//! exports a chapter's or a volume's pages to a staging directory, runs
//! `kcc-c2e` on it there, and moves the single file KCC wrote to its final
//! name in one rename, so a reader never sees a half-written file. Stored
//! pages are never touched.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::KccConfig;
use crate::db::SourceDatabase;

/// One build: a title, its author, and the chapters in reading order. Pages
/// are read from `db` one chapter at a time while exporting, so a volume's
/// pages are never all in memory at once.
pub struct KccJob<'a> {
    pub title: String,
    pub author: &'a str,
    /// Chapter name and id, in reading order
    pub chapters: Vec<(String, isize)>,
    pub db: &'a SourceDatabase,
}

/// Why a KCC build produced nothing usable
#[derive(Debug)]
pub enum KccError {
    Io(std::io::Error),
    Db(rusqlite::Error),
    /// KCC exited with an error; `output` is the tail of what it printed
    Failed { status: Option<i32>, output: String },
    /// KCC split the output over the size ceiling; the pieces, with sizes, were discarded
    Split(Vec<(String, u64)>),
    /// KCC exited cleanly and wrote nothing
    NoOutput,
    TimedOut,
}

impl fmt::Display for KccError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KccError::Io(e) => write!(f, "{}", e),
            KccError::Db(e) => write!(f, "{}", e),
            KccError::Failed { status, output } => {
                write!(f, "kcc-c2e exited with status {:?}: {}", status, output.trim())
            }
            KccError::Split(parts) => {
                let listed: Vec<String> = parts
                    .iter()
                    .map(|(name, size)| format!("{} ({} MB)", name, size / (1024 * 1024)))
                    .collect();
                write!(
                    f,
                    "kcc-c2e split the output over the size ceiling into {}; lower the quality with Manga.Kcc.ExtraArgs or split the volume in VolumeEnds",
                    listed.join(", ")
                )
            }
            KccError::NoOutput => write!(f, "kcc-c2e wrote no output file"),
            KccError::TimedOut => write!(f, "kcc-c2e ran past the time limit and was stopped"),
        }
    }
}

impl std::error::Error for KccError {}

impl From<std::io::Error> for KccError {
    fn from(e: std::io::Error) -> Self {
        KccError::Io(e)
    }
}

impl From<rusqlite::Error> for KccError {
    fn from(e: rusqlite::Error) -> Self {
        KccError::Db(e)
    }
}

const TIME_LIMIT: Duration = Duration::from_secs(30 * 60);

/// A directory or file name KCC and a filesystem will accept
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() { "untitled".to_string() } else { trimmed.to_string() }
}

/// Build `job` with KCC and leave the result at `destination`, replacing
/// any file there in one rename. `splitter` is KCC's double-page mode: 0
/// split, 1 rotate, 2 both. Staging happens in a dot-directory beside the
/// destination, removed on every return path.
pub fn build_with_kcc(
    kcc: &KccConfig,
    splitter: u8,
    job: &KccJob,
    destination: &Path,
) -> Result<(), KccError> {
    let out_dir = destination.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(out_dir)?;
    let title = safe_name(&job.title);
    let staging = out_dir.join(format!(".kcc-{}-{}", std::process::id(), title));
    let result = export_and_run(kcc, splitter, job, &title, &staging, destination);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn export_and_run(
    kcc: &KccConfig,
    splitter: u8,
    job: &KccJob,
    title: &str,
    staging: &Path,
    destination: &Path,
) -> Result<(), KccError> {
    // KCC names its output after the input directory, so the export
    // directory carries the title. Its output goes to `out`, beside it.
    let input = staging.join("in").join(title);
    let out = staging.join("out");
    std::fs::create_dir_all(&input)?;
    std::fs::create_dir_all(&out)?;
    for (n, (name, chapter_id)) in job.chapters.iter().enumerate() {
        let chapter_dir = input.join(format!("{:03} - {}", n + 1, safe_name(name)));
        std::fs::create_dir_all(&chapter_dir)?;
        for (i, page) in job.db.get_chapter_images(*chapter_id)?.iter().enumerate() {
            let ext = page.path.rsplit('.').next().unwrap_or("jpg");
            let mut file = std::fs::File::create(chapter_dir.join(format!("{:03}.{}", i + 1, ext)))?;
            file.write_all(&page.data)?;
        }
    }

    // KCC's own output goes to a file, not a pipe: a chatty run must not
    // block on a full pipe and look like a hang.
    let log_path = staging.join("kcc.log");
    let log = std::fs::File::create(&log_path)?;
    let log_err = log.try_clone()?;
    let mut command = Command::new(&kcc.command);
    command
        .arg("-p").arg(&kcc.profile)
        .arg("-f").arg(&kcc.format)
        .arg("-m")
        .arg("-r").arg(splitter.to_string())
        .arg("--ts").arg(kcc.target_mb.to_string())
        .arg("-t").arg(&job.title)
        .arg("-a").arg(job.author)
        .arg("-o").arg(&out);
    if kcc.colour {
        command.arg("--forcecolor");
    }
    command
        .args(&kcc.extra_args)
        .arg(&input)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let child = command.spawn()?;
    let Some(status) = wait_with_limit(child, Instant::now())? else {
        return Err(KccError::TimedOut);
    };
    if !status.success() {
        let printed = std::fs::read_to_string(&log_path).unwrap_or_default();
        let tail: Vec<&str> = printed.lines().rev().take(8).collect();
        let output = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(KccError::Failed { status: status.code(), output });
    }

    // "<title> 1.epub", "<title> 2.epub" ... is KCC's split over --ts.
    let mut parts = Vec::new();
    let mut single = None;
    for entry in std::fs::read_dir(&out)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_part = name
            .strip_prefix(&format!("{} ", title))
            .and_then(|rest| rest.strip_suffix(".epub"))
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty());
        if is_part {
            parts.push((name, entry.metadata()?.len()));
        } else if name == format!("{}.epub", title) {
            single = Some(entry.path());
        }
    }
    if !parts.is_empty() {
        parts.sort();
        return Err(KccError::Split(parts));
    }
    let Some(built) = single else {
        return Err(KccError::NoOutput);
    };
    std::fs::rename(&built, destination)?;
    Ok(())
}

/// Wait for KCC, killing it at the time limit. `None` means it was killed.
fn wait_with_limit(mut child: std::process::Child, started: Instant) -> std::io::Result<Option<std::process::ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() > TIME_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ChapterImage, RegenerateFlags};
    use crate::test_support::fake_kcc;

    /// Two chapters with two and one pages, in an in-memory database
    fn db_with_pages() -> SourceDatabase {
        let db = SourceDatabase::open_in_memory("t").unwrap();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("Chapter 1", "https://example.com/c1", vol).unwrap();
        db.add_chapter("Chapter 2: The/Return", "https://example.com/c2", vol).unwrap();
        let chapters = db.get_chapters_by_volume(vol).unwrap();
        let page = |chapter_id: isize, seq: u32, ext: &str| ChapterImage {
            chapter_id,
            seq,
            path: format!("images/{}-{}.{}", chapter_id, seq, ext),
            source_url: "https://example.com/p".into(),
            mime: "image/jpeg".into(),
            width: 1,
            height: 1,
            data: vec![seq as u8],
        };
        db.store_chapter_content(chapters[0].id, "<p/>", &[page(chapters[0].id, 0, "jpg"), page(chapters[0].id, 1, "png")], RegenerateFlags::ChapterOnly).unwrap();
        db.store_chapter_content(chapters[1].id, "<p/>", &[page(chapters[1].id, 0, "jpg")], RegenerateFlags::ChapterOnly).unwrap();
        db
    }

    fn job(db: &SourceDatabase) -> KccJob<'_> {
        let chapters = db.get_chapters_by_volume(db.add_volume("Volume 1").unwrap()).unwrap();
        KccJob {
            title: "Example Series Volume 1".into(),
            author: "Example Author",
            chapters: chapters.into_iter().map(|c| (c.name, c.id)).collect(),
            db,
        }
    }

    fn config(command: String) -> KccConfig {
        KccConfig { command, extra_args: vec!["--jpeg-quality".into(), "75".into()], ..KccConfig::default() }
    }

    #[test]
    fn export_lays_out_chapters_and_pages_in_order_and_passes_the_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let kcc = config(fake_kcc(dir.path(), "ok"));
        let db = db_with_pages();
        let destination = out.join("JoJo's Example, Volume 1.epub");

        build_with_kcc(&kcc, 1, &job(&db), &destination).unwrap();

        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "fake", "the file lands at the caller's name, however odd");
        let args: Vec<String> = std::fs::read_to_string(dir.path().join("args.txt")).unwrap().lines().map(String::from).collect();
        let input = args.last().unwrap().clone();
        let staging_out = args[args.iter().position(|a| a == "-o").unwrap() + 1].clone();
        assert_eq!(
            &args[..args.len() - 1],
            &["-p", "KCS", "-f", "KFX", "-m", "-r", "1", "--ts", "195", "-t", "Example Series Volume 1", "-a", "Example Author", "-o", staging_out.as_str(), "--forcecolor", "--jpeg-quality", "75"]
        );
        assert!(input.ends_with("/in/Example Series Volume 1"), "{}", input);
        assert!(staging_out.ends_with("/out") && !Path::new(&staging_out).exists(), "KCC writes into staging, which is removed: {}", staging_out);
        let listing = std::fs::read_to_string(dir.path().join("listing.txt")).unwrap();
        for expected in ["001 - Chapter 1", "002 - Chapter 2_ The_Return", "001.jpg", "002.png"] {
            assert!(listing.contains(expected), "{} missing from:\n{}", expected, listing);
        }
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "only the epub remains in the output directory");
    }

    #[test]
    fn a_split_output_is_an_error_naming_the_sizes_and_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let destination = out.join("Example Series Volume 1.epub");
        std::fs::write(&destination, b"previous").unwrap();
        let kcc = config(fake_kcc(dir.path(), "split"));
        let db = db_with_pages();

        let err = build_with_kcc(&kcc, 0, &job(&db), &destination).unwrap_err();

        match &err {
            KccError::Split(parts) => assert_eq!(parts, &[("Example Series Volume 1 1.epub".to_string(), 7), ("Example Series Volume 1 2.epub".to_string(), 7)]),
            other => panic!("{:?}", other),
        }
        assert!(err.to_string().contains("VolumeEnds"), "{}", err);
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous", "a failed build leaves the old file alone");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "the pieces and the staging are gone");
    }

    #[test]
    fn a_failing_command_reports_what_it_printed() {
        let dir = tempfile::tempdir().unwrap();
        let kcc = config(fake_kcc(dir.path(), "fail"));
        let db = db_with_pages();
        let err = build_with_kcc(&kcc, 0, &job(&db), &dir.path().join("out").join("x.epub")).unwrap_err();
        match err {
            KccError::Failed { status, output } => {
                assert_eq!(status, Some(3));
                assert!(output.contains("boom"), "{}", output);
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn a_clean_exit_without_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let kcc = config(fake_kcc(dir.path(), "silent"));
        let db = db_with_pages();
        assert!(matches!(build_with_kcc(&kcc, 0, &job(&db), &dir.path().join("out").join("x.epub")), Err(KccError::NoOutput)));
    }

    #[test]
    fn a_missing_command_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let kcc = config(dir.path().join("nope").to_string_lossy().into_owned());
        let db = db_with_pages();
        assert!(matches!(build_with_kcc(&kcc, 0, &job(&db), &dir.path().join("out").join("x.epub")), Err(KccError::Io(_))));
    }
}
