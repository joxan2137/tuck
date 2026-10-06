//! Pure text helpers for cards: relative times, hosts, previews, file summaries.

use std::path::Path;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Wall-clock reference for relative times: Unix ms and the local UTC offset in minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub now_ms: i64,
    pub utc_offset_minutes: i32,
}

impl Clock {
    fn local_day(&self, ms: i64) -> i64 {
        (ms + self.utc_offset_minutes as i64 * MINUTE_MS).div_euclid(DAY_MS)
    }
}

/// (year, month 1-12, day) of a day count since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// "now", "2 min", "3 h" (earlier today), "Yesterday", "3 Oct", "3 Oct 2025".
pub fn relative_time(then_ms: i64, clock: Clock) -> String {
    let age = clock.now_ms - then_ms;
    if age < MINUTE_MS {
        return "now".into();
    }
    if age < HOUR_MS {
        return format!("{} min", age / MINUTE_MS);
    }
    let (today, then_day) = (clock.local_day(clock.now_ms), clock.local_day(then_ms));
    if then_day == today {
        return format!("{} h", age / HOUR_MS);
    }
    if then_day == today - 1 {
        return "Yesterday".into();
    }
    let (year, month, day) = civil_from_days(then_day);
    let (this_year, ..) = civil_from_days(today);
    let month = MONTHS[(month - 1) as usize];
    if year == this_year { format!("{day} {month}") } else { format!("{day} {month} {year}") }
}

/// The host of a URL without scheme, credentials, port, path or a leading "www.".
pub fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = host.split(':').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host)
}

/// At most `max_lines` lines and `max_chars` characters of `text` for a card preview: trimmed, tabs as four
/// spaces, the common indentation removed, trailing spaces dropped.
pub fn preview_text(text: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<String> = text
        .trim_matches(|c: char| c == '\n' || c == '\r')
        .lines()
        .take(max_lines)
        .map(|line| line.replace('\t', "    ").trim_end().to_string())
        .collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.get(indent..).unwrap_or(line.trim_start()));
        if out.chars().count() >= max_chars {
            break;
        }
    }
    let out = out.trim_end();
    match out.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", out[..cut].trim_end()),
        None => out.to_string(),
    }
}

/// File names to list (up to `max_names`) and how many more paths there are.
pub fn file_summary(paths: &[std::path::PathBuf], max_names: usize) -> (Vec<String>, usize) {
    let shown = if paths.len() > max_names { max_names.saturating_sub(1).max(1) } else { paths.len() };
    let names = paths.iter().take(shown).map(|p| file_name(p)).collect();
    (names, paths.len() - shown)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// True for paths that name a folder by convention (trailing separator or no extension and no dot).
pub fn looks_like_folder(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.ends_with(['\\', '/']) || path.extension().is_none()
}

pub fn dimensions(width: u32, height: u32) -> String {
    format!("{width} × {height}")
}

pub fn item_count(total: usize) -> String {
    if total == 1 { "1 item".into() } else { format!("{total} items") }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const NOON_2026_10_06: i64 = 1_791_288_000_000;

    fn clock() -> Clock {
        Clock { now_ms: NOON_2026_10_06, utc_offset_minutes: 120 }
    }

    #[test]
    fn civil_dates_round_trip_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(NOON_2026_10_06.div_euclid(DAY_MS)), (2026, 10, 6));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn relative_times_read_naturally() {
        let c = clock();
        assert_eq!(relative_time(c.now_ms - 20_000, c), "now");
        assert_eq!(relative_time(c.now_ms - 2 * MINUTE_MS, c), "2 min");
        assert_eq!(relative_time(c.now_ms - 3 * HOUR_MS, c), "3 h");
        assert_eq!(relative_time(c.now_ms - 20 * HOUR_MS, c), "Yesterday");
        assert_eq!(relative_time(c.now_ms - 3 * DAY_MS, c), "3 Oct");
        assert_eq!(relative_time(c.now_ms - 400 * DAY_MS, c), "1 Sep 2025");
    }

    #[test]
    fn local_midnight_decides_yesterday() {
        let c = Clock { now_ms: NOON_2026_10_06 - 13 * HOUR_MS, utc_offset_minutes: 120 };
        assert_eq!(relative_time(c.now_ms - 2 * HOUR_MS, c), "Yesterday", "01:00 local minus 2 h is yesterday");
        let utc = Clock { utc_offset_minutes: 0, ..c };
        assert_eq!(relative_time(utc.now_ms - 2 * HOUR_MS, utc), "2 h");
    }

    #[test]
    fn hosts_strip_everything_but_the_name() {
        assert_eq!(url_host("https://www.apple.com/design/?q=1"), "apple.com");
        assert_eq!(url_host("http://user:pw@example.org:8080/x"), "example.org");
        assert_eq!(url_host("www.rust-lang.org"), "rust-lang.org");
        assert_eq!(url_host("ftp://files.example.net"), "files.example.net");
    }

    #[test]
    fn previews_dedent_and_cap() {
        assert_eq!(preview_text("\n    fn a() {\n        b();\n    }\n", 8, 500), "fn a() {\n    b();\n}");
        assert_eq!(preview_text("a\tb", 4, 100), "a    b");
        assert_eq!(preview_text("one two three", 4, 7), "one two…");
        assert_eq!(preview_text("1\n2\n3\n4\n5\n6", 3, 100), "1\n2\n3");
    }

    #[test]
    fn file_summaries_name_the_first_files() {
        let paths: Vec<PathBuf> = ["C:\\a\\report.xlsx", "C:\\a\\photo.heic", "C:\\a\\notes.txt", "C:\\a\\b.txt"].map(PathBuf::from).to_vec();
        assert_eq!(file_summary(&paths, 3), (vec!["report.xlsx".to_string(), "photo.heic".to_string()], 2));
        assert_eq!(file_summary(&paths[..3], 3).1, 0);
        assert!(looks_like_folder(Path::new("C:\\Users\\me\\Documents")));
        assert!(!looks_like_folder(Path::new("C:\\a\\notes.txt")));
        assert_eq!(item_count(1), "1 item");
        assert_eq!(dimensions(1920, 1080), "1920 × 1080");
    }
}
