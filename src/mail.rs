use async_trait::async_trait;
use crate::config::{MailConfig, UserConfig};
use mail_builder::MessageBuilder;
use mail_send::SmtpClientBuilder;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}
impl Default for Attachment {
    fn default() -> Self {
        Attachment {
            filename: String::default(),
            mime: String::from("application/epub+zip"),
            bytes: Vec::<u8>::default(),
        }
    }
}

async fn send_epub(config: &MailConfig, dest: &UserConfig, attachment: &Attachment) {
    let message = MessageBuilder::new()
        .from((config.name.clone(), config.address.clone()))
        .to(vec![(dest.name.clone(), dest.email.clone())])
        .attachment(
            attachment.mime.clone(),
            attachment.filename.clone(),
            attachment.bytes.clone(),
        );

    let res = SmtpClientBuilder::new(config.smtp_hostname.clone(), config.smtp_port)
        .expect("Failed to create SMTP client")
        .implicit_tls(false)
        .credentials((config.address.clone(), config.password.clone()))
        .connect()
        .await
        .unwrap()
        .send(message)
        .await;

    match res {
        Ok(()) => println!(
            "Sent Chapter {} to {}",
            attachment.filename.clone(),
            dest.email,
        ),
        Err(error) => panic!("Problem with sending email {:?}", error),
    };
}

pub async fn send_epubs(
    config: &MailConfig,
    source_id: &str,
    volumes: &Vec<Attachment>,
    volumes_s: &Vec<Attachment>,
    chapters: &Vec<Attachment>,
    chapters_s: &Vec<Attachment>,
) {
    for dest in config.destinations.clone() {
        if !dest.receives_source(source_id) {
            continue;
        }
        let src_cfg = dest.source_config(source_id);
        if src_cfg.send_full_volumes {
            for vol in volumes {
                send_epub(config, &dest, vol).await;
            }
            if src_cfg.strip_colour {
                for vol in volumes_s {
                    send_epub(config, &dest, vol).await;
                }
            }
        }
        if src_cfg.send_individual_chapters {
            for chapter in chapters {
                send_epub(config, &dest, chapter).await;
            }
            if src_cfg.strip_colour {
                for chapter in chapters_s {
                    send_epub(config, &dest, chapter).await;
                }
            }
        }
    }
}

/// Where one message goes.
#[derive(Clone, Debug)]
pub struct Recipient {
    pub name: String,
    pub email: String,
}

/// Why a manual send failed, as shown on the status page and in the log.
///
/// Deliberately fixed strings. `mail_send::Error` can carry the server's
/// reply text, so it is matched on and dropped, never formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Could not reach the server, or the connection dropped.
    Connect,
    /// The server refused the login.
    Login,
    /// The server refused this message.
    Rejected,
    Other,
}

impl SendError {
    pub fn message(self) -> &'static str {
        match self {
            SendError::Connect => "could not reach the mail server",
            SendError::Login => "the mail server refused the login",
            SendError::Rejected => "the mail server refused the message",
            SendError::Other => "sending failed",
        }
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Connect,
    Send,
}

fn classify(error: &mail_send::Error, phase: Phase) -> SendError {
    use mail_send::Error as E;
    match error {
        E::Io(_) | E::Tls(_) | E::Timeout | E::InvalidTLSName | E::MissingStartTls | E::UnparseableReply => {
            SendError::Connect
        }
        E::Auth(_) | E::AuthenticationFailed(_) | E::MissingCredentials | E::UnsupportedAuthMechanism => {
            SendError::Login
        }
        E::UnexpectedReply(_) => match phase {
            Phase::Connect => SendError::Connect,
            Phase::Send => SendError::Rejected,
        },
        _ => SendError::Other,
    }
}

/// Sends one EPUB per message. `Connect` and `Login` mean nothing further
/// can be sent on this job; `Rejected` and `Other` concern one message.
#[async_trait]
pub trait Mailer: Send {
    async fn send(&mut self, to: &Recipient, attachment: &Attachment) -> Result<(), SendError>;
}

/// An open SMTP session, whatever its stream type. Lets `SmtpMailer` keep a
/// connection without naming `tokio_rustls`' stream type.
///
/// `Sync` (not just `Send`) so `Box<dyn Connection>` inside `SmtpMailer`
/// keeps `SmtpMailer` itself `Sync`: `Mailer::send`'s `&self` borrow of
/// `connect()` is held across an `.await`, and `#[async_trait]` requires the
/// resulting future to be `Send`, which needs that borrow's referent `Sync`.
#[async_trait]
trait Connection: Send + Sync {
    async fn send_message(&mut self, message: MessageBuilder<'static>) -> mail_send::Result<()>;
}

#[async_trait]
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync> Connection for mail_send::SmtpClient<T> {
    async fn send_message(&mut self, message: MessageBuilder<'static>) -> mail_send::Result<()> {
        self.send(message).await
    }
}

/// One connection per job, opened on the first message and reopened once if
/// it drops.
///
/// The reconnect-once retry (see `Mailer::send` below) can then deliver a
/// message twice, if the server accepted it but its final reply was lost to
/// the dropped connection. That is preferred to dropping the message.
pub struct SmtpMailer {
    config: MailConfig,
    connection: Option<Box<dyn Connection>>,
}

impl std::fmt::Debug for SmtpMailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `MailConfig`'s own Debug already hides the password.
        f.debug_struct("SmtpMailer").field("config", &self.config).finish_non_exhaustive()
    }
}

impl SmtpMailer {
    pub fn new(config: MailConfig) -> Self {
        SmtpMailer { config, connection: None }
    }

    async fn connect(&self) -> Result<Box<dyn Connection>, SendError> {
        let client = SmtpClientBuilder::new(self.config.smtp_hostname.clone(), self.config.smtp_port)
            .map_err(|_| SendError::Connect)?
            // A manual send holds the one job slot, so a server that stops
            // answering must not hold it for the library default of an hour
            // per command.
            .timeout(std::time::Duration::from_secs(60))
            .implicit_tls(false)
            .credentials((self.config.address.clone(), self.config.password.clone()))
            .connect()
            .await
            .map_err(|e| classify(&e, Phase::Connect))?;
        Ok(Box::new(client))
    }

    fn message(&self, to: &Recipient, attachment: &Attachment) -> MessageBuilder<'static> {
        MessageBuilder::new()
            .from((self.config.name.clone(), self.config.address.clone()))
            .to(vec![(to.name.clone(), to.email.clone())])
            .attachment(
                attachment.mime.clone(),
                attachment.filename.clone(),
                attachment.bytes.clone(),
            )
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&mut self, to: &Recipient, attachment: &Attachment) -> Result<(), SendError> {
        for attempt in 0..2 {
            if self.connection.is_none() {
                self.connection = Some(self.connect().await?);
            }
            let message = self.message(to, attachment);
            let connection = self.connection.as_mut().expect("connected above");
            match connection.send_message(message).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let error = classify(&e, Phase::Send);
                    self.connection = None;
                    if error == SendError::Connect && attempt == 0 {
                        continue;
                    }
                    return Err(error);
                }
            }
        }
        Err(SendError::Connect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_faults_are_connect_and_login_faults_are_login() {
        let io = mail_send::Error::Io(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "x"));
        assert_eq!(classify(&io, Phase::Connect), SendError::Connect);
        assert_eq!(classify(&mail_send::Error::Timeout, Phase::Connect), SendError::Connect);
        assert_eq!(classify(&mail_send::Error::MissingCredentials, Phase::Connect), SendError::Login);
        assert_eq!(classify(&mail_send::Error::UnsupportedAuthMechanism, Phase::Connect), SendError::Login);
        // A dropped connection mid-send is a connection fault, so the job
        // can reconnect once rather than fail every remaining message.
        assert_eq!(classify(&mail_send::Error::Timeout, Phase::Send), SendError::Connect);
        assert_eq!(classify(&mail_send::Error::MissingRcptTo, Phase::Send), SendError::Other);
    }

    #[test]
    fn messages_are_fixed_text() {
        for e in [SendError::Connect, SendError::Login, SendError::Rejected, SendError::Other] {
            assert!(!e.message().is_empty());
            assert_eq!(e.to_string(), e.message());
        }
    }

    #[test]
    fn a_recipient_debug_does_not_matter_but_the_mailer_debug_hides_the_password() {
        let mailer = SmtpMailer::new(MailConfig {
            name: "Example Sender".into(),
            address: "sender@example.com".into(),
            password: "synthetic-pw-918273".into(),
            smtp_hostname: "smtp.example.com".into(),
            smtp_port: 587,
            destinations: vec![],
        });
        assert!(!format!("{:?}", mailer).contains("synthetic-pw-918273"));
    }
}
