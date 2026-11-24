//! Manga EPUBs through Kindle Comic Converter.
//!
//! KCC does what a comic reader needs and this crate does not: pages sized
//! to the device, double-page spreads split in right-to-left order, a size
//! ceiling, and an output Amazon's Send to Kindle converts well. The scraper
//! exports a chapter's or a volume's pages to a temporary directory, runs
//! `kcc-c2e` on it, and keeps the single file KCC writes. Stored pages are
//! never touched.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::KccConfig;
use crate::db::ChapterImage;

/// One build: a title, its author, and the chapters' pages in reading order
pub struct KccJob<'a> {
    pub title: String,
    pub author: &'a str,
    /// Chapter name and its pages in `seq` order
    pub chapters: Vec<(String, Vec<ChapterImage>)>,
}

/// Why a KCC build produced nothing usable
#[derive(Debug)]
pub enum KccError {
    Io(std::io::Error),
    /// KCC exited with an error; `stderr` is its last lines
    Failed { status: Option<i32>, stderr: String },
    /// KCC split the output over the size ceiling; the pieces, with sizes, were deleted
    Split(Vec<(String, u64)>),
    /// KCC exited cleanly and wrote nothing
    NoOutput,
    TimedOut,
}

impl fmt::Display for KccError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KccError::Io(e) => write!(f, "{}", e),
            KccError::Failed { status, stderr } => {
                write!(f, "kcc-c2e exited with status {:?}: {}", status, stderr.trim())
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

/// Build `job` with KCC into `out_dir` and return the file it wrote.
/// `splitter` is KCC's double-page mode: 0 split, 1 rotate, 2 both.
pub fn build_with_kcc(
    kcc: &KccConfig,
    splitter: u8,
    job: &KccJob,
    out_dir: &Path,
) -> Result<PathBuf, KccError> {
    std::fs::create_dir_all(out_dir)?;
    let title = safe_name(&job.title);
    let export = out_dir.join(format!(".kcc-{}-{}", std::process::id(), title));
    let result = export_and_run(kcc, splitter, job, &title, &export, out_dir);
    let _ = std::fs::remove_dir_all(&export);
    result
}

fn export_and_run(
    kcc: &KccConfig,
    splitter: u8,
    job: &KccJob,
    title: &str,
    export: &Path,
    out_dir: &Path,
) -> Result<PathBuf, KccError> {
    // KCC names its output after the input directory, so the export
    // directory carries the title.
    let input = export.join(title);
    std::fs::create_dir_all(&input)?;
    for (n, (name, pages)) in job.chapters.iter().enumerate() {
        let chapter_dir = input.join(format!("{:03} - {}", n + 1, safe_name(name)));
        std::fs::create_dir_all(&chapter_dir)?;
        for (i, page) in pages.iter().enumerate() {
            let ext = page.path.rsplit('.').next().unwrap_or("jpg");
            let mut file = std::fs::File::create(chapter_dir.join(format!("{:03}.{}", i + 1, ext)))?;
            file.write_all(&page.data)?;
        }
    }

    // Whatever KCC leaves with the title's name is ours to judge below.
    let output = out_dir.join(format!("{}.epub", title));
    let _ = std::fs::remove_file(&output);

    let mut command = Command::new(&kcc.command);
    command
        .arg("-p").arg(&kcc.profile)
        .arg("-f").arg(&kcc.format)
        .arg("-m")
        .arg("-r").arg(splitter.to_string())
        .arg("--ts").arg(kcc.target_mb.to_string())
        .arg("-t").arg(&job.title)
        .arg("-a").arg(job.author)
        .arg("-o").arg(out_dir)
        .args(&kcc.extra_args)
        .arg(&input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn()?;
    let started = Instant::now();
    let finished = wait_with_limit(child, started)?;
    let Some(finished) = finished else {
        return Err(KccError::TimedOut);
    };
    if !finished.status.success() {
        let stderr = String::from_utf8_lossy(&finished.stderr);
        let stdout = String::from_utf8_lossy(&finished.stdout);
        let tail: String = stderr.lines().chain(stdout.lines()).rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(KccError::Failed { status: finished.status.code(), stderr: tail });
    }

    // "<title> 1.epub", "<title> 2.epub" ... is KCC's split over --ts.
    let mut parts = Vec::new();
    for entry in std::fs::read_dir(out_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_part = name
            .strip_prefix(&format!("{} ", title))
            .and_then(|rest| rest.strip_suffix(".epub"))
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty());
        if is_part {
            parts.push((name, entry.metadata()?.len()));
        }
    }
    if !parts.is_empty() {
        for (name, _) in &parts {
            let _ = std::fs::remove_file(out_dir.join(name));
        }
        let _ = std::fs::remove_file(&output);
        parts.sort();
        return Err(KccError::Split(parts));
    }
    if !output.is_file() {
        return Err(KccError::NoOutput);
    }
    Ok(output)
}

/// Wait for KCC, killing it at the time limit. `None` means it was killed.
fn wait_with_limit(mut child: std::process::Child, started: Instant) -> std::io::Result<Option<std::process::Output>> {
    loop {
        if child.try_wait()?.is_some() {
            return Ok(Some(child.wait_with_output()?));
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
    use crate::test_support::fake_kcc;

    #[allow(dead_code)]
    fn unused_fake_kcc(dir: &Path, mode: &str) -> String {
        let script = dir.join("kcc-c2e");
        let body = format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > "{args}"
out=""
while [ $# -gt 1 ]; do
  if [ "$1" = "-o" ]; then out="$2"; fi
  shift
done
input="$1"
name=$(basename "$input")
ls -R "$input" > "{listing}"
case "{mode}" in
  ok) printf 'fake' > "$out/$name.epub" ;;
  split) printf 'fakeone' > "$out/$name 1.epub"; printf 'faketwo' > "$out/$name 2.epub" ;;
  fail) echo boom >&2; exit 3 ;;
  silent) ;;
esac
"#,
            args = dir.join("args.txt").display(),
            listing = dir.join("listing.txt").display(),
            mode = mode
        );
        std::fs::write(&script, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    fn page(seq: u32, ext: &str) -> ChapterImage {
        ChapterImage {
            chapter_id: 1,
            seq,
            path: format!("images/1-{}.{}", seq, ext),
            source_url: "https://example.com/p".into(),
            mime: "image/jpeg".into(),
            width: 1,
            height: 1,
            data: vec![seq as u8],
        }
    }

    fn job() -> KccJob<'static> {
        KccJob {
            title: "Example Series Volume 1".into(),
            author: "Example Author",
            chapters: vec![
                ("Chapter 1".into(), vec![page(0, "jpg"), page(1, "png")]),
                ("Chapter 2: The/Return".into(), vec![page(0, "jpg")]),
            ],
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

        let built = build_with_kcc(&kcc, 1, &job(), &out).unwrap();

        assert_eq!(built, out.join("Example Series Volume 1.epub"));
        assert_eq!(std::fs::read_to_string(&built).unwrap(), "fake");
        let args: Vec<String> = std::fs::read_to_string(dir.path().join("args.txt")).unwrap().lines().map(String::from).collect();
        let input = args.last().unwrap().clone();
        assert_eq!(
            &args[..args.len() - 1],
            &["-p", "KCS", "-f", "KFX", "-m", "-r", "1", "--ts", "195", "-t", "Example Series Volume 1", "-a", "Example Author", "-o", out.to_str().unwrap(), "--jpeg-quality", "75"]
        );
        assert!(input.ends_with("/Example Series Volume 1"), "{}", input);
        let listing = std::fs::read_to_string(dir.path().join("listing.txt")).unwrap();
        for expected in ["001 - Chapter 1", "002 - Chapter 2_ The_Return", "001.jpg", "002.png"] {
            assert!(listing.contains(expected), "{} missing from:\n{}", expected, listing);
        }
        assert!(!Path::new(&input).exists(), "the export is removed afterwards");
        assert!(std::fs::read_dir(&out).unwrap().count() == 1, "only the epub remains in the output directory");
    }

    #[test]
    fn a_split_output_is_an_error_naming_the_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let kcc = config(fake_kcc(dir.path(), "split"));

        let err = build_with_kcc(&kcc, 0, &job(), &out).unwrap_err();

        match &err {
            KccError::Split(parts) => assert_eq!(parts, &[("Example Series Volume 1 1.epub".to_string(), 7), ("Example Series Volume 1 2.epub".to_string(), 7)]),
            other => panic!("{:?}", other),
        }
        assert!(err.to_string().contains("VolumeEnds"), "{}", err);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "the pieces are removed");
    }

    #[test]
    fn a_failing_command_reports_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let kcc = config(fake_kcc(dir.path(), "fail"));
        let err = build_with_kcc(&kcc, 0, &job(), &dir.path().join("out")).unwrap_err();
        match err {
            KccError::Failed { status, stderr } => {
                assert_eq!(status, Some(3));
                assert!(stderr.contains("boom"), "{}", stderr);
            }
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn a_clean_exit_without_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let kcc = config(fake_kcc(dir.path(), "silent"));
        assert!(matches!(build_with_kcc(&kcc, 0, &job(), &dir.path().join("out")), Err(KccError::NoOutput)));
    }

    #[test]
    fn a_missing_command_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let kcc = config(dir.path().join("nope").to_string_lossy().into_owned());
        assert!(matches!(build_with_kcc(&kcc, 0, &job(), &dir.path().join("out")), Err(KccError::Io(_))));
    }
}
