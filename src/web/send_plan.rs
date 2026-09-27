//! What a manual send will do, worked out before anything is sent.
//!
//! Pure functions over the source's statistics and the mail settings, so
//! every rule the send form enforces is testable without a server.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::config::MailConfig;
use crate::mail::Recipient;
use crate::stats::cache::SourceStat;
use crate::web::toc::format_thousands;

/// Emails per job, items times destinations. Stops a mis-click from sending
/// a whole series chapter by chapter.
pub const MAX_EMAILS: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Volume { id: i64, name: String },
    Chapter { id: i64, name: String },
}

impl Item {
    pub fn label(&self) -> &str {
        match self {
            Item::Volume { name, .. } | Item::Chapter { name, .. } => name,
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub volumes: Vec<i64>,
    pub chapters: Vec<i64>,
}

/// Decode an `application/x-www-form-urlencoded` body or query string.
/// Repeated keys are kept, which `serde_urlencoded` cannot do.
pub fn form_pairs(raw: &[u8]) -> Vec<(String, String)> {
    form_urlencoded::parse(raw).into_owned().collect()
}

impl Selection {
    pub fn parse(pairs: &[(String, String)]) -> Self {
        let mut sel = Selection::default();
        for (key, value) in pairs {
            let Ok(id) = value.parse::<i64>() else { continue };
            let list = match key.as_str() {
                "v" => &mut sel.volumes,
                "c" => &mut sel.chapters,
                _ => continue,
            };
            if !list.contains(&id) {
                list.push(id);
            }
        }
        sel
    }
}

pub struct ItemLine {
    pub item: Item,
    pub detail: String,
}

pub struct Resolved {
    pub items: Vec<ItemLine>,
    /// Ticked chapters left out because their ticked volume contains them.
    pub inside_volume: Vec<String>,
    /// Ticked ids that do not exist, are not downloaded, or are a volume
    /// with nothing downloaded.
    pub unavailable: usize,
}

pub fn resolve(stat: &SourceStat, selection: &Selection) -> Resolved {
    let mut items = Vec::new();
    let mut inside_volume = Vec::new();
    let mut found = 0;

    for volume in &stat.volumes {
        let id = volume.id as i64;
        let downloaded = volume.chapters.len() - volume.pending_chapters;
        if selection.volumes.contains(&id) && downloaded > 0 {
            found += 1;
            items.push(ItemLine {
                item: Item::Volume { id, name: volume.name.clone() },
                detail: format!("{} chapters, {} words", downloaded, format_thousands(volume.words)),
            });
        }
    }
    let ticked_volumes: Vec<i64> = items
        .iter()
        .filter_map(|l| match l.item { Item::Volume { id, .. } => Some(id), _ => None })
        .collect();

    for volume in &stat.volumes {
        let absorbed = ticked_volumes.contains(&(volume.id as i64));
        for chapter in &volume.chapters {
            let id = chapter.id as i64;
            if !selection.chapters.contains(&id) || !chapter.downloaded {
                continue;
            }
            found += 1;
            if absorbed {
                inside_volume.push(chapter.name.clone());
            } else {
                items.push(ItemLine {
                    item: Item::Chapter { id, name: chapter.name.clone() },
                    detail: format!("{} words", format_thousands(chapter.words)),
                });
            }
        }
    }

    Resolved {
        items,
        inside_volume,
        unavailable: selection.volumes.len() + selection.chapters.len() - found,
    }
}

/// Identifies the destination list a form was built from: names and
/// addresses, in order. A POST whose fingerprint differs is refused, so an
/// index can never land on an address other than the one shown.
pub fn fingerprint(mail: &MailConfig) -> String {
    let mut h = DefaultHasher::new();
    for d in &mail.destinations {
        d.name.hash(&mut h);
        d.email.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DestChoice {
    pub index: usize,
    pub strip_colour: bool,
}

pub fn choices(pairs: &[(String, String)]) -> Vec<DestChoice> {
    let mut out: Vec<DestChoice> = Vec::new();
    for (key, value) in pairs {
        if key != "d" {
            continue;
        }
        let Ok(index) = value.parse::<usize>() else { continue };
        if out.iter().any(|c| c.index == index) {
            continue;
        }
        let colour_key = format!("colour-{}", index);
        let strip_colour = pairs.iter().any(|(k, v)| *k == colour_key && v == "stripped");
        out.push(DestChoice { index, strip_colour });
    }
    out.sort_by_key(|c| c.index);
    out
}

#[derive(Clone, Debug)]
pub struct PlannedEmail {
    pub item: Item,
    pub dest_name: String,
    pub to: Recipient,
    pub strip_colour: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NothingSelected,
    NoDestination,
    TooMany(usize),
    ConfigChanged,
    ConfigNotLoaded,
    Busy,
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::NothingSelected => "Nothing to send. Tick volumes or chapters on the contents page.".into(),
            Refusal::NoDestination => "Tick at least one destination.".into(),
            Refusal::TooMany(n) => format!(
                "That is {} emails; the limit is {} per send. Send fewer at a time.",
                n, MAX_EMAILS
            ),
            Refusal::ConfigChanged => {
                "The destination list changed since this form was opened. Reload it.".into()
            }
            Refusal::ConfigNotLoaded => "The configuration file does not load right now, so nothing \
                can be sent. Fix it on the Configuration page first."
                .into(),
            Refusal::Busy => "A send is in progress. Wait for it to finish.".into(),
        }
    }
}

pub fn plan(
    items: &[ItemLine],
    mail: &MailConfig,
    fingerprint_seen: &str,
    choices: &[DestChoice],
) -> Result<Vec<PlannedEmail>, Refusal> {
    if fingerprint_seen != fingerprint(mail) {
        return Err(Refusal::ConfigChanged);
    }
    if items.is_empty() {
        return Err(Refusal::NothingSelected);
    }
    if choices.is_empty() {
        return Err(Refusal::NoDestination);
    }
    if choices.iter().any(|c| c.index >= mail.destinations.len()) {
        return Err(Refusal::ConfigChanged);
    }
    let total = items.len() * choices.len();
    if total > MAX_EMAILS {
        return Err(Refusal::TooMany(total));
    }
    let mut out = Vec::with_capacity(total);
    for choice in choices {
        let dest = &mail.destinations[choice.index];
        for line in items {
            out.push(PlannedEmail {
                item: line.item.clone(),
                dest_name: dest.name.clone(),
                to: Recipient { name: dest.name.clone(), email: dest.email.clone() },
                strip_colour: choice.strip_colour,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MailConfig, UserConfig};
    use crate::stats::cache::{ChapterStat, SourceStat, VolumeStat};

    fn chapter(id: isize, name: &str, words: usize, downloaded: bool) -> ChapterStat {
        ChapterStat {
            id,
            name: name.into(),
            uri: format!("https://example.com/{}", id),
            words,
            published: None,
            downloaded,
        }
    }

    /// Two volumes. Volume 2's last chapter is pending. Volume 3 has only a
    /// pending chapter, so there is nothing in it to send.
    fn stat() -> SourceStat {
        let v1 = vec![chapter(10, "1.01 Start", 1000, true), chapter(11, "1.02 Next", 2000, true)];
        let v2 = vec![chapter(20, "2.01 Later", 3000, true), chapter(21, "2.02 Soon", 0, false)];
        let v3 = vec![chapter(30, "3.01 Unwritten", 0, false)];
        SourceStat {
            source_id: "test-source".into(),
            name: "Test Serial".into(),
            volumes: vec![
                VolumeStat { id: 1, name: "Volume 1".into(), chapters: v1, words: 3000, pending_chapters: 0 },
                VolumeStat { id: 2, name: "Volume 2".into(), chapters: v2, words: 3000, pending_chapters: 1 },
                VolumeStat { id: 3, name: "Volume 3".into(), chapters: v3, words: 0, pending_chapters: 1 },
            ],
            ..SourceStat::empty_for_test()
        }
    }

    fn mail() -> MailConfig {
        let dest = |name: &str, email: &str, strip: bool| UserConfig {
            name: name.into(),
            email: email.into(),
            strip_colour: strip,
            ..Default::default()
        };
        MailConfig {
            name: "Example Sender".into(),
            address: "sender@example.com".into(),
            password: "synthetic-pw-918273".into(),
            smtp_hostname: "smtp.example.com".into(),
            smtp_port: 587,
            destinations: vec![
                dest("Test Reader", "reader@example.com", true),
                dest("Second Reader", "second@example.org", false),
            ],
        }
    }

    fn pairs(s: &str) -> Vec<(String, String)> {
        form_pairs(s.as_bytes())
    }

    #[test]
    fn a_selection_reads_v_and_c_and_ignores_the_rest() {
        let sel = Selection::parse(&pairs("v=2&c=11&v=abc&x=1&c=11&v=1"));
        assert_eq!(sel, Selection { volumes: vec![2, 1], chapters: vec![11] });
    }

    #[test]
    fn items_come_in_contents_order_and_a_ticked_volume_absorbs_its_chapters() {
        let r = resolve(&stat(), &Selection::parse(&pairs("c=20&v=1&c=11&c=21&c=999&v=3")));
        let labels: Vec<_> = r.items.iter().map(|l| l.item.label().to_string()).collect();
        assert_eq!(labels, ["Volume 1", "2.01 Later"]);
        assert_eq!(r.inside_volume, ["1.02 Next"]);
        // 21 is pending, 999 does not exist, volume 3 has nothing downloaded.
        assert_eq!(r.unavailable, 3);
        assert_eq!(r.items[0].detail, "2 chapters, 3,000 words");
        assert_eq!(r.items[1].detail, "3,000 words");
    }

    #[test]
    fn a_plan_is_each_destination_times_each_item_with_its_variant() {
        let r = resolve(&stat(), &Selection::parse(&pairs("v=1&c=20")));
        let m = mail();
        let got = plan(&r.items, &m, &fingerprint(&m), &choices(&pairs("d=1&d=0&colour-0=stripped&colour-1=colour"))).unwrap();
        let summary: Vec<_> = got.iter().map(|e| (e.dest_name.as_str(), e.item.label(), e.strip_colour)).collect();
        assert_eq!(summary, [
            ("Test Reader", "Volume 1", true),
            ("Test Reader", "2.01 Later", true),
            ("Second Reader", "Volume 1", false),
            ("Second Reader", "2.01 Later", false),
        ]);
        assert_eq!(got[0].to.email, "reader@example.com");
    }

    #[test]
    fn refusals() {
        let m = mail();
        let fp = fingerprint(&m);
        let r = resolve(&stat(), &Selection::parse(&pairs("v=1")));
        assert_eq!(plan(&[], &m, &fp, &choices(&pairs("d=0"))).unwrap_err(), Refusal::NothingSelected);
        assert_eq!(plan(&r.items, &m, &fp, &[]).unwrap_err(), Refusal::NoDestination);
        assert_eq!(plan(&r.items, &m, "stale", &choices(&pairs("d=0"))).unwrap_err(), Refusal::ConfigChanged);
        assert_eq!(plan(&r.items, &m, &fp, &choices(&pairs("d=7"))).unwrap_err(), Refusal::ConfigChanged);

        let many: Vec<ItemLine> = (0..26)
            .map(|i| ItemLine { item: Item::Chapter { id: i, name: format!("c{}", i) }, detail: String::new() })
            .collect();
        assert_eq!(plan(&many, &m, &fp, &choices(&pairs("d=0&d=1"))).unwrap_err(), Refusal::TooMany(52));
        assert_eq!(plan(&many[..25], &m, &fp, &choices(&pairs("d=0&d=1"))).unwrap().len(), MAX_EMAILS);
    }

    #[test]
    fn the_fingerprint_changes_with_an_address_or_the_order_and_not_with_other_settings() {
        let m = mail();
        let fp = fingerprint(&m);
        let mut moved = mail();
        moved.destinations[0].email = "elsewhere@example.com".into();
        assert_ne!(fingerprint(&moved), fp);
        let mut swapped = mail();
        swapped.destinations.swap(0, 1);
        assert_ne!(fingerprint(&swapped), fp);
        let mut toggled = mail();
        toggled.destinations[0].strip_colour = false;
        toggled.password = "other".into();
        assert_eq!(fingerprint(&toggled), fp);
    }

    #[test]
    fn refusal_messages_do_not_mention_addresses() {
        for r in [Refusal::NothingSelected, Refusal::NoDestination, Refusal::TooMany(60),
                  Refusal::ConfigChanged, Refusal::ConfigNotLoaded, Refusal::Busy] {
            assert!(!r.message().contains('@'), "{}", r.message());
        }
        assert!(Refusal::TooMany(60).message().contains("60"));
    }
}
