//! Manual send jobs: an in-memory record of what was sent, and the task that
//! sends it. A restart clears the record, as it does sessions.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Semaphore;

use crate::config::SourceConfig;
use crate::mail::{Attachment, Mailer, SendError};
use crate::web::auth::random_token;
use crate::web::download::{build_chapter, build_volume, BuildError};
use crate::web::send_plan::{Item, PlannedEmail, Refusal};

/// Finished jobs kept for the status page, the running one included.
pub const HISTORY: usize = 20;

const BUILD_FAILED: &str = "could not build the EPUB";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmailState {
    Waiting,
    Building,
    Sending,
    Sent,
    Failed(&'static str),
}

#[derive(Clone, Debug)]
pub struct JobEmail {
    pub item: String,
    pub dest_name: String,
    pub strip_colour: bool,
    pub state: EmailState,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub source_id: String,
    pub source_name: String,
    pub started: chrono::DateTime<chrono::Local>,
    pub emails: Vec<JobEmail>,
    pub finished: bool,
}

impl Job {
    /// (sent, failed, total)
    pub fn counts(&self) -> (usize, usize, usize) {
        let sent = self.emails.iter().filter(|e| e.state == EmailState::Sent).count();
        let failed = self.emails.iter().filter(|e| matches!(e.state, EmailState::Failed(_))).count();
        (sent, failed, self.emails.len())
    }
}

/// Newest first. At most one job is unfinished, and if there is one it is
/// at the front.
pub struct SendJobs {
    jobs: Mutex<VecDeque<Job>>,
}

impl SendJobs {
    pub fn new() -> Self {
        SendJobs { jobs: Mutex::new(VecDeque::new()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn try_start(&self, source_id: &str, source_name: &str, emails: &[PlannedEmail]) -> Result<String, Refusal> {
        let mut jobs = self.lock();
        if jobs.front().map_or(false, |j| !j.finished) {
            return Err(Refusal::Busy);
        }
        let id = random_token();
        jobs.push_front(Job {
            id: id.clone(),
            source_id: source_id.to_string(),
            source_name: source_name.to_string(),
            started: chrono::Local::now(),
            emails: emails
                .iter()
                .map(|e| JobEmail {
                    item: e.item.label().to_string(),
                    dest_name: e.dest_name.clone(),
                    strip_colour: e.strip_colour,
                    state: EmailState::Waiting,
                })
                .collect(),
            finished: false,
        });
        jobs.truncate(HISTORY);
        Ok(id)
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.lock().iter().find(|j| j.id == id).cloned()
    }

    pub fn latest(&self) -> Option<Job> {
        self.lock().front().cloned()
    }

    pub fn is_running(&self) -> bool {
        self.lock().front().map_or(false, |j| !j.finished)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    fn set(&self, id: &str, index: usize, state: EmailState) {
        if let Some(job) = self.lock().iter_mut().find(|j| j.id == id) {
            job.emails[index].state = state;
        }
    }

    /// Marks the job finished; any email still waiting or in progress is
    /// failed, so a job that ended early never shows as still going.
    pub(crate) fn finish(&self, id: &str) {
        if let Some(job) = self.lock().iter_mut().find(|j| j.id == id) {
            for e in &mut job.emails {
                if matches!(e.state, EmailState::Waiting | EmailState::Building | EmailState::Sending) {
                    e.state = EmailState::Failed("the send stopped before this email");
                }
            }
            job.finished = true;
        }
    }
}

// `pub(crate)`, not `pub`: `BuildError` is itself `pub(crate)` (see
// `download.rs`), and a `pub` item naming a less-visible type is a
// `private_interfaces` warning. Nothing outside the crate needs these —
// `send.rs` (inside `src/web`) is the only caller.
pub(crate) type BuildFn = fn(&SourceConfig, &Item, bool) -> Result<Attachment, BuildError>;

/// The same builds the download links serve.
pub(crate) fn build_item(source: &SourceConfig, item: &Item, strip_colour: bool) -> Result<Attachment, BuildError> {
    match item {
        Item::Volume { id, .. } => build_volume(source, *id, strip_colour),
        Item::Chapter { id, .. } => build_chapter(source, *id, strip_colour),
    }
}

/// Finishes the job however `run_job` ends, a panic included, so a failure
/// can never leave a job that blocks every later send until a restart.
struct FinishOnDrop {
    jobs: Arc<SendJobs>,
    id: String,
}

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        self.jobs.finish(&self.id);
    }
}

pub(crate) async fn run_job(
    jobs: Arc<SendJobs>,
    id: String,
    source: SourceConfig,
    emails: Vec<PlannedEmail>,
    mut mailer: Box<dyn Mailer>,
    build: BuildFn,
    permits: Arc<Semaphore>,
) {
    let _finish = FinishOnDrop { jobs: Arc::clone(&jobs), id: id.clone() };
    let source = Arc::new(source);
    let mut fatal: Option<SendError> = None;

    for (index, email) in emails.iter().enumerate() {
        if let Some(error) = fatal {
            jobs.set(&id, index, EmailState::Failed(error.message()));
            continue;
        }

        jobs.set(&id, index, EmailState::Building);
        let built = {
            // The same cap the download links share, so a send cannot starve them
            // or be starved indefinitely.
            let Ok(_permit) = Arc::clone(&permits).acquire_owned().await else { return };
            let (source, item, strip) = (Arc::clone(&source), email.item.clone(), email.strip_colour);
            tokio::task::spawn_blocking(move || build(&source, &item, strip)).await
        };
        let attachment = match built {
            Ok(Ok(a)) => a,
            Ok(Err(_)) | Err(_) => {
                jobs.set(&id, index, EmailState::Failed(BUILD_FAILED));
                log(&id, &source.id, email, BUILD_FAILED);
                continue;
            }
        };

        jobs.set(&id, index, EmailState::Sending);
        match mailer.send(&email.to, &attachment).await {
            Ok(()) => {
                jobs.set(&id, index, EmailState::Sent);
                log(&id, &source.id, email, "sent");
            }
            Err(error) => {
                jobs.set(&id, index, EmailState::Failed(error.message()));
                log(&id, &source.id, email, error.message());
                if matches!(error, SendError::Connect | SendError::Login) {
                    fatal = Some(error);
                }
            }
        }
    }
}

/// One line per attempt, for the journal. The destination's name, never its
/// address; the outcome is a fixed string.
fn log(job: &str, source_id: &str, email: &PlannedEmail, outcome: &str) {
    println!(
        "manual send {}: {} \"{}\" ({}) to {}: {}",
        &job[..8],
        source_id,
        email.item.label(),
        if email.strip_colour { "stripped" } else { "colour" },
        email.dest_name,
        outcome
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::{Recipient, SendError};
    use crate::test_support::RecordingMailer;
    use crate::web::send_plan::{Item, PlannedEmail};

    fn email(item: &str, to: &str, strip: bool) -> PlannedEmail {
        PlannedEmail {
            item: Item::Chapter { id: item.len() as i64, name: item.into() },
            dest_name: to.into(),
            to: Recipient { name: to.into(), email: format!("{}@example.com", to.to_lowercase()) },
            strip_colour: strip,
        }
    }

    fn source() -> SourceConfig {
        SourceConfig { id: "test-source".into(), name: "Test Serial".into(), enabled: true, ..SourceConfig::default() }
    }

    fn fake_build(_: &SourceConfig, item: &Item, strip: bool) -> Result<Attachment, BuildError> {
        if item.label() == "broken" {
            return Err(BuildError::Internal);
        }
        Ok(Attachment {
            filename: format!("{}{}.epub", item.label(), if strip { "-s" } else { "" }),
            ..Attachment::default()
        })
    }

    async fn run(emails: Vec<PlannedEmail>, mailer: RecordingMailer) -> Job {
        let jobs = Arc::new(SendJobs::new());
        let id = jobs.try_start("test-source", "Test Serial", &emails).unwrap();
        run_job(Arc::clone(&jobs), id.clone(), source(), emails, Box::new(mailer), fake_build,
                Arc::new(Semaphore::new(1))).await;
        jobs.get(&id).unwrap()
    }

    #[tokio::test]
    async fn every_email_is_sent_in_order_with_its_variant() {
        let (mailer, sent) = RecordingMailer::new();
        let job = run(vec![email("a", "Reader", true), email("b", "Reader", true), email("a", "Other", false)], mailer).await;
        assert!(job.finished);
        assert_eq!(job.counts(), (3, 0, 3));
        let got: Vec<_> = sent.lock().unwrap().iter().map(|s| (s.to.clone(), s.filename.clone())).collect();
        assert_eq!(got, [
            ("reader@example.com".to_string(), "a-s.epub".to_string()),
            ("reader@example.com".to_string(), "b-s.epub".to_string()),
            ("other@example.com".to_string(), "a.epub".to_string()),
        ]);
    }

    #[tokio::test]
    async fn a_build_failure_or_a_rejection_fails_one_email_only() {
        let (mailer, sent) = RecordingMailer::failing_on(1, SendError::Rejected);
        let job = run(vec![email("broken", "Reader", false), email("a", "Reader", false),
                           email("b", "Reader", false), email("c", "Reader", false)], mailer).await;
        let states: Vec<_> = job.emails.iter().map(|e| e.state.clone()).collect();
        assert_eq!(states, [
            EmailState::Failed("could not build the EPUB"),
            EmailState::Sent,
            EmailState::Failed(SendError::Rejected.message()),
            EmailState::Sent,
        ]);
        assert_eq!(sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_connection_or_login_failure_fails_the_rest_with_one_reason() {
        for error in [SendError::Connect, SendError::Login] {
            let (mailer, sent) = RecordingMailer::failing_on(1, error);
            let job = run(vec![email("a", "R", false), email("b", "R", false), email("c", "R", false)], mailer).await;
            let states: Vec<_> = job.emails.iter().map(|e| e.state.clone()).collect();
            assert_eq!(states, [EmailState::Sent, EmailState::Failed(error.message()), EmailState::Failed(error.message())]);
            assert_eq!(sent.lock().unwrap().len(), 1);
            assert!(job.finished);
        }
    }

    #[test]
    fn only_one_job_runs_at_a_time_and_history_is_bounded() {
        let jobs = SendJobs::new();
        let first = jobs.try_start("test-source", "Test Serial", &[email("a", "R", false)]).unwrap();
        assert_eq!(jobs.try_start("test-source", "Test Serial", &[email("a", "R", false)]), Err(Refusal::Busy));
        assert!(jobs.is_running());
        jobs.finish(&first);
        assert!(!jobs.is_running());
        for _ in 0..HISTORY + 5 {
            let id = jobs.try_start("test-source", "Test Serial", &[email("a", "R", false)]).unwrap();
            jobs.finish(&id);
        }
        assert!(jobs.get(&first).is_none(), "the oldest job has been dropped");
        assert_eq!(jobs.len(), HISTORY);
    }

    #[test]
    fn job_ids_are_unguessable_tokens() {
        let jobs = SendJobs::new();
        let id = jobs.try_start("test-source", "Test Serial", &[email("a", "R", false)]).unwrap();
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
