use crate::config::{MailConfig, UserConfig};
use mail_builder::MessageBuilder;
use mail_send::SmtpClientBuilder;

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
