//! The scrape schedule, read from a system crontab file for display only.
//!
//! The file is named explicitly (`--schedule-file`) rather than discovered:
//! guessing which crontab holds the job is how a page ends up showing a
//! schedule that is not the one running.
//!
//! Only the timing fields ever leave this module. Cron commands routinely
//! carry secrets, a healthcheck ping URL most often, so the command is
//! parsed to decide whether a line runs the scraper and is then dropped.

use std::path::Path;

use chrono::{DateTime, TimeZone};
use croner::parser::{CronParser, Seconds};

/// The installed binary is named after the package; there is no separate
/// `[[bin]]` name to read.
const BINARY: &str = env!("CARGO_PKG_NAME");

/// One crontab line that runs a scrape.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledRun<Tz: TimeZone> {
    /// The timing fields as written, e.g. `17 * * * *` or `@hourly`.
    pub expression: String,
    /// A plain-English reading where the pattern is a common one, e.g.
    /// "hourly at :17"; otherwise the expression itself.
    pub summary: String,
    /// When it next runs, in the zone `now` was given in. `None` for
    /// `@reboot`, or if the expression could not be evaluated.
    pub next: Option<DateTime<Tz>>,
}

/// Every line in `path` that runs a scrape.
pub fn read_schedule<Tz: TimeZone>(
    path: &Path,
    now: &DateTime<Tz>,
) -> Result<Vec<ScheduledRun<Tz>>, std::io::Error> {
    Ok(parse_schedule(&std::fs::read_to_string(path)?, now))
}

/// Every line in `text`, in system crontab format (`/etc/crontab`,
/// `/etc/cron.d/*`: five timing fields, then a user, then the command),
/// that runs a scrape. Comments, environment settings, other jobs, web
/// service invocations and lines too short to be entries are skipped.
pub fn parse_schedule<Tz: TimeZone>(text: &str, now: &DateTime<Tz>) -> Vec<ScheduledRun<Tz>> {
    text.lines().filter_map(|line| parse_line(line, now)).collect()
}

fn parse_line<Tz: TimeZone>(line: &str, now: &DateTime<Tz>) -> Option<ScheduledRun<Tz>> {
    let line = line.trim();
    // An entry starts with a minute field or an @nickname. Anything else is
    // a comment, an environment setting (`MAILTO=...`), or not an entry.
    let first = line.chars().next()?;
    if !(first.is_ascii_digit() || first == '*' || first == '@') {
        return None;
    }

    let tokens: Vec<&str> = line.split_whitespace().collect();
    let timing_fields = if first == '@' { 1 } else { 5 };
    // Timing, then the user, then at least one word of command.
    if tokens.len() < timing_fields + 2 {
        return None;
    }
    let (timing, rest) = tokens.split_at(timing_fields);
    let command = &rest[1..];
    if !runs_a_scrape(command) {
        return None;
    }

    let expression = timing.join(" ");
    if expression.eq_ignore_ascii_case("@reboot") {
        return Some(ScheduledRun {
            summary: "at boot".to_string(),
            expression,
            next: None,
        });
    }

    let next = CronParser::builder()
        .seconds(Seconds::Disallowed)
        .build()
        .parse(&expression)
        .ok()
        .and_then(|cron| cron.find_next_occurrence(now, false).ok());
    Some(ScheduledRun {
        summary: summarise(timing),
        expression,
        next,
    })
}

/// Whether a command runs the scraper, as opposed to its web service or
/// something else entirely: some word of it is the binary, by bare name or
/// by path, and the word after that is not the `web` subcommand.
///
/// Words are split at shell punctuation as well as whitespace. Cron commands
/// are shell, and `.../wandering_inn_scraper; curl ...` or `sh -c "..."` put
/// a separator or a quote against the binary's name with no space between.
fn runs_a_scrape(command: &[&str]) -> bool {
    const SHELL_PUNCTUATION: &[char] = &[';', '&', '|', '(', ')', '<', '>', '"', '\'', '`'];
    let words: Vec<&str> = command
        .iter()
        .flat_map(|w| w.split(SHELL_PUNCTUATION))
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().enumerate().any(|(i, word)| {
        let name = word.rsplit('/').next().unwrap_or(word);
        name == BINARY && words.get(i + 1) != Some(&"web")
    })
}

/// "hourly at :17", "daily at 03:00", or the expression unchanged when it is
/// anything less common. A partial English rendering of cron is worse than
/// none, so only the shapes that read unambiguously are translated.
fn summarise(timing: &[&str]) -> String {
    let number = |f: &str| f.parse::<u32>().ok();
    match timing {
        [n] if n.eq_ignore_ascii_case("@hourly") => "hourly at :00".to_string(),
        [n] if n.eq_ignore_ascii_case("@daily") || n.eq_ignore_ascii_case("@midnight") => {
            "daily at 00:00".to_string()
        }
        [m, "*", "*", "*", "*"] if number(m).map_or(false, |m| m < 60) => {
            format!("hourly at :{:02}", number(m).unwrap())
        }
        [m, h, "*", "*", "*"]
            if number(m).map_or(false, |m| m < 60) && number(h).map_or(false, |h| h < 24) =>
        {
            format!("daily at {:02}:{:02}", number(h).unwrap(), number(m).unwrap())
        }
        _ => timing.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().with_timezone(&Utc)
    }

    /// Shaped like a real cron.d file for this service: comments, an
    /// environment line, a scrape that pings a healthcheck with a secret
    /// token, the web service, and an unrelated job.
    const CRON_D: &str = "\
# Scraper jobs
MAILTO=ops@example.com
17 * * * * scraper cd /var/lib/scraper && /usr/bin/wandering_inn_scraper >> scraper.log 2>&1; curl -fsS https://hc.example.com/ping/0f5e8c1a-secret-token
*/5 * * * * scraper /usr/bin/wandering_inn_scraper web --bind 127.0.0.1:8080
0 3 * * * root /usr/sbin/logrotate /etc/logrotate.conf
";

    #[test]
    fn finds_the_scrape_and_nothing_else() {
        let runs = parse_schedule(CRON_D, &at("2026-09-26T18:05:00Z"));
        assert_eq!(runs.len(), 1, "{:?}", runs);
        assert_eq!(runs[0].expression, "17 * * * *");
        assert_eq!(runs[0].summary, "hourly at :17");
        assert_eq!(runs[0].next, Some(at("2026-09-26T18:17:00Z")));
    }

    #[test]
    fn nothing_from_the_command_survives_parsing() {
        let runs = parse_schedule(CRON_D, &at("2026-09-26T18:05:00Z"));
        let rendered = format!("{:?}", runs);
        for secret in ["secret-token", "hc.example.com", "/var/lib/scraper", "scraper.log"] {
            assert!(!rendered.contains(secret), "{} leaked: {}", secret, rendered);
        }
    }

    /// The binary's name with shell punctuation against it, as real cron
    /// lines write it.
    #[test]
    fn the_binary_is_found_through_shell_punctuation() {
        let now = at("2026-09-26T18:05:00Z");
        for command in [
            "/usr/bin/wandering_inn_scraper; curl -fsS https://hc.example.com/ping/x",
            "cd /var/lib/scraper&&/usr/bin/wandering_inn_scraper&&curl https://hc.example.com/ping/x",
            "/usr/bin/wandering_inn_scraper>>scraper.log 2>&1",
            "sh -c \"cd /var/lib/scraper && /usr/bin/wandering_inn_scraper\"",
            "(cd /var/lib/scraper; wandering_inn_scraper)",
        ] {
            let line = format!("17 * * * * scraper {}", command);
            assert_eq!(parse_schedule(&line, &now).len(), 1, "not found in: {}", command);
        }
        // And the web service is still told apart when punctuation follows.
        assert!(parse_schedule(
            "@reboot scraper /usr/bin/wandering_inn_scraper web --bind 127.0.0.1:8080;",
            &now
        )
        .is_empty());
    }

    /// The shape a deployed entry actually has: a `cd`, a `flock` with its
    /// own arguments in front of the binary, output redirection, and a
    /// healthcheck ping carrying the exit status after it. The binary is
    /// found in the middle, and nothing else on the line survives.
    #[test]
    fn a_compound_entry_with_flock_and_a_ping_is_recognised_and_dropped() {
        let line = "17 * * * * scraper cd /var/lib/scraper && flock -n /var/lock/scraper.lock \
                    /usr/bin/wandering_inn_scraper >> /var/lib/scraper/scraper.log 2>&1; \
                    curl -fsS -m 10 --retry 5 https://hc.example.com/ping/0f5e8c1a-secret-token/$? \
                    >/dev/null 2>&1";
        let runs = parse_schedule(line, &at("2026-09-26T18:05:00Z"));
        assert_eq!(runs.len(), 1, "{:?}", runs);
        assert_eq!(runs[0].summary, "hourly at :17");
        assert_eq!(runs[0].next, Some(at("2026-09-26T18:17:00Z")));

        let rendered = format!("{:?}", runs);
        for secret in ["secret-token", "hc.example.com", "/var/lib/scraper", "scraper.lock", "flock"] {
            assert!(!rendered.contains(secret), "{} leaked: {}", secret, rendered);
        }
    }

    #[test]
    fn a_daily_run_rolls_over_midnight() {
        let runs = parse_schedule(
            "30 2 * * * scraper /usr/bin/wandering_inn_scraper",
            &at("2026-09-26T23:00:00Z"),
        );
        assert_eq!(runs[0].summary, "daily at 02:30");
        assert_eq!(runs[0].next, Some(at("2026-09-27T02:30:00Z")));
    }

    #[test]
    fn nicknames_are_understood() {
        let runs = parse_schedule(
            "@hourly scraper wandering_inn_scraper --source example-source",
            &at("2026-09-26T18:05:00Z"),
        );
        assert_eq!(runs[0].expression, "@hourly");
        assert_eq!(runs[0].summary, "hourly at :00");
        assert_eq!(runs[0].next, Some(at("2026-09-26T19:00:00Z")));
    }

    #[test]
    fn an_uncommon_pattern_is_shown_as_written_and_still_timed() {
        let runs = parse_schedule(
            "*/20 9-17 * * 1-5 scraper /usr/bin/wandering_inn_scraper",
            // A Saturday, so the next run is Monday at 09:00.
            &at("2026-09-26T18:05:00Z"),
        );
        assert_eq!(runs[0].summary, "*/20 9-17 * * 1-5");
        assert_eq!(runs[0].next, Some(at("2026-09-28T09:00:00Z")));
    }

    /// Debian's cron runs a line whose day-of-month AND day-of-week are both
    /// restricted when EITHER matches. Some cron libraries require both. The
    /// page would then show a next run later than the real one, so this pins
    /// the behaviour against a future upgrade of the parser.
    #[test]
    fn day_of_month_and_day_of_week_match_either_way_as_cron_does() {
        let runs = parse_schedule(
            // Noon on the 1st, or noon on any Monday.
            "0 12 1 * 1 scraper /usr/bin/wandering_inn_scraper",
            // Saturday the 26th: the next Monday (28th) comes before the 1st.
            &at("2026-09-26T18:05:00Z"),
        );
        assert_eq!(runs[0].next, Some(at("2026-09-28T12:00:00Z")));
    }

    #[test]
    fn reboot_has_no_next_time() {
        let runs = parse_schedule(
            "@reboot scraper /usr/bin/wandering_inn_scraper",
            &at("2026-09-26T18:05:00Z"),
        );
        assert_eq!(runs[0].summary, "at boot");
        assert_eq!(runs[0].next, None);
    }

    #[test]
    fn lines_that_are_not_entries_are_skipped_not_fatal() {
        let text = "\
17 * * scraper /usr/bin/wandering_inn_scraper
17 * * * *
not a cron line at all
99 99 * * * scraper /usr/bin/wandering_inn_scraper
";
        let runs = parse_schedule(text, &at("2026-09-26T18:05:00Z"));
        // Only the last line has the shape of an entry. Its fields are out
        // of range, so it is listed, as written, with no next time.
        assert_eq!(runs.len(), 1, "{:?}", runs);
        assert_eq!(runs[0].summary, "99 99 * * *");
        assert_eq!(runs[0].next, None);
    }
}
