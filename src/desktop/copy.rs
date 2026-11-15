//! User-facing wording that several surfaces share: counts, decision
//! summaries and plain-language error text. Everything here is a pure
//! function so the copy is unit-tested in one place.
use super::NoticeKind;
use shadowroom::duplicates::{DuplicateScanState, ScanPhase};
use shadowroom::model::{ColorLabel, Flag};

/// A count with its noun: "1 photo", "0 photos", "50,112 photos".
pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

/// A count with comma thousands separators: "7", "2,113", "50,112".
pub(super) fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A grouping gap in words: "0.5 s", "2 s", "60 min".
pub(super) fn duration(ms: u64) -> String {
    if ms < 60_000 {
        let seconds = ms as f64 / 1000.;
        if ms.is_multiple_of(1000) {
            format!("{seconds:.0} s")
        } else {
            format!("{seconds:.1} s")
        }
    } else {
        format!("{} min", (ms + 30_000) / 60_000)
    }
}

/// Rating-threshold wording for the filter menu and smart collection.
pub(super) fn rating_threshold(rating: u8) -> String {
    match rating {
        0 => "All ratings".into(),
        5 => "5 stars".into(),
        r => format!("{} and up", plural(usize::from(r), "star", "stars")),
    }
}

/// The decision a photo now carries, for confirmations: "Pick · ★★★ · Red".
/// A decision with nothing set reads "Cleared".
pub(super) fn decision_summary(flag: Flag, rating: u8, label: ColorLabel) -> String {
    let mut parts = Vec::with_capacity(3);
    match flag {
        Flag::Pick => parts.push("Pick".to_owned()),
        Flag::Reject => parts.push("Reject".to_owned()),
        Flag::None => {}
    }
    if rating > 0 {
        parts.push(super::stars(rating).to_owned());
    }
    if label != ColorLabel::None {
        parts.push(label.as_str().to_owned());
    }
    if parts.is_empty() {
        "Cleared".into()
    } else {
        parts.join(" · ")
    }
}

/// What a batch decision does, as a verb phrase and its object:
/// ("Reject", "Rejected", " · ★★"), used by the question and the outcome.
fn batch_parts(
    rating: Option<u8>,
    flag: Option<Flag>,
    label: Option<ColorLabel>,
) -> (&'static str, &'static str, String) {
    match (rating, flag, label) {
        (Some(0), Some(Flag::None), None) => ("Reset", "Reset", String::new()),
        (None, Some(Flag::Pick), None) => ("Pick", "Picked", String::new()),
        (None, Some(Flag::Reject), None) => ("Reject", "Rejected", String::new()),
        (None, Some(Flag::None), None) => {
            ("Clear the flag of", "Cleared the flag of", String::new())
        }
        (Some(0), None, None) => (
            "Clear the rating of",
            "Cleared the rating of",
            String::new(),
        ),
        (Some(r), None, None) => ("Rate", "Rated", format!(" {}", super::stars(r))),
        (None, None, Some(ColorLabel::None)) => {
            ("Clear the label of", "Cleared the label of", String::new())
        }
        (None, None, Some(l)) => ("Label", "Labelled", format!(" {}", l.as_str())),
        _ => ("Change", "Changed", String::new()),
    }
}

/// "Reject 1,200 photos?" before a held batch decision is applied.
pub(super) fn batch_question(
    rating: Option<u8>,
    flag: Option<Flag>,
    label: Option<ColorLabel>,
    n: usize,
) -> String {
    let (verb, _, object) = batch_parts(rating, flag, label);
    format!("{verb} {}{object}?", plural(n, "photo", "photos"))
}

/// "Rejected 1,200 photos" after a large batch decision.
pub(super) fn batch_done(
    rating: Option<u8>,
    flag: Option<Flag>,
    label: Option<ColorLabel>,
    n: usize,
) -> String {
    let (_, done, object) = batch_parts(rating, flag, label);
    format!("{done} {}{object}", plural(n, "photo", "photos"))
}

/// Which photos an action touched: the file name for one, a count otherwise.
pub(super) fn subject(count: usize, filename: Option<&str>) -> String {
    match (count, filename) {
        (1, Some(name)) => name.to_owned(),
        _ => plural(count, "photo", "photos"),
    }
}

/// Decimal sizes with one decimal below 10 units: "512 B", "2.4 MB", "48 GB".
pub(super) fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000. && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10. {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// The import button: "Import 1 photo (2.4 MB)", videos only when present.
pub(super) fn import_label(photos: usize, videos: usize, bytes: u64) -> String {
    let mut label = format!("Import {}", plural(photos, "photo", "photos"));
    if videos > 0 {
        label.push_str(&format!(" · {}", plural(videos, "video", "videos")));
    }
    label.push_str(&format!(" ({})", fmt_bytes(bytes)));
    label
}

/// The Duplicates view with nothing to show, by scan state: what happened,
/// what was checked, and the button that moves on (None while scanning).
pub(super) fn duplicates_empty(
    scan: Option<&DuplicateScanState>,
    scope: &str,
) -> (String, String, Option<&'static str>) {
    let Some(scan) = scan else {
        return (
            "Review duplicate photos".into(),
            format!(
                "Scan {scope} to verify exact file matches. Nothing is moved or removed automatically."
            ),
            Some("Find Duplicates"),
        );
    };
    match scan.phase {
        ScanPhase::Queued | ScanPhase::Running => (
            "Checking for duplicates…".into(),
            format!(
                "Checked {} of {} candidates {scope}.",
                scan.processed, scan.candidates
            ),
            None,
        ),
        ScanPhase::Completed => (
            "No exact duplicates".into(),
            format!(
                "Checked {} {scope}. Nothing is moved or removed automatically.",
                plural(scan.total, "photo", "photos")
            ),
            Some("Scan again"),
        ),
        ScanPhase::Stale => (
            "Duplicate results are out of date".into(),
            "Photos changed since the last scan. Scan again to verify exact matches.".into(),
            Some("Scan again"),
        ),
        ScanPhase::Cancelled => (
            "Duplicate scan cancelled".into(),
            format!("No results were kept. Scan {scope} again when you are ready."),
            Some("Scan again"),
        ),
        ScanPhase::Failed => (
            "Duplicate scan failed".into(),
            scan.errors
                .first()
                .cloned()
                .unwrap_or_else(|| "The scan stopped before it finished.".into()),
            Some("Retry"),
        ),
    }
}

/// The error toast's text, split into a plain headline, an explanation and
/// (when the headline dropped anything) the original message for Details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ToastCopy {
    pub title: String,
    pub detail: Option<String>,
    pub raw: Option<String>,
}

/// Plain explanations for storage failures that otherwise surface as raw
/// SQLite or OS text ("database is locked: Error code 5: …").
fn plain_cause(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    Some(if has("database is locked") || has("database is busy") {
        "The catalog is busy (database locked)."
    } else if has("readonly database") || has("read-only file system") {
        "The catalog is read-only."
    } else if has("database or disk is full") || has("no space left on device") {
        "The disk is full."
    } else if has("malformed") && has("database") {
        "The catalog file is damaged."
    } else if has("disk i/o error") || has("input/output error") {
        "The disk reported a read or write error."
    } else if has("permission denied") {
        "Permission was denied."
    } else if has("no such file or directory") {
        "A file or folder is missing."
    } else {
        return None;
    })
}

/// "Error code 5" and similar engine codes never belong in a headline.
fn is_code(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    lower
        .strip_prefix("error code ")
        .is_some_and(|code| !code.is_empty() && code.chars().all(|c| c.is_ascii_digit()))
}

/// Splits `context: context: cause` chains, dropping engine codes.
fn segments(text: &str) -> Vec<&str> {
    text.split(": ")
        .map(str::trim)
        .filter(|s| !s.is_empty() && !is_code(s))
        .collect()
}

/// Areas that prefix their failures ("Import: …"); their headline says what failed.
fn area_title(area: &str) -> Option<String> {
    const AREAS: [&str; 6] = [
        "Import",
        "Export",
        "File picker",
        "Theme import",
        "Duplicate scan",
        "Deletion",
    ];
    AREAS.contains(&area).then(|| format!("{area} failed"))
}

/// Plain-language toast copy: no doubled prefixes or raw engine codes in the
/// headline. `unsaved` counts the photos whose decisions are not durable.
pub(super) fn toast_copy(kind: NoticeKind, text: &str, unsaved: usize) -> ToastCopy {
    let text = text.trim();
    let cause = plain_cause(text);
    let parts = segments(text);
    let lossy = cause.is_some() || parts.join(": ") != text;
    let last = parts.last().copied().unwrap_or(text);
    let (title, detail) = match kind {
        NoticeKind::UnsavedDecisions => (
            if unsaved == 0 {
                "Couldn't save decisions".to_owned()
            } else {
                format!("Couldn't save {}", plural(unsaved, "decision", "decisions"))
            },
            format!(
                "{} Your changes are kept in this window.",
                cause.map_or_else(|| sentence(last), str::to_owned)
            ),
        ),
        NoticeKind::UnsavedPreferences => (
            "Couldn't save preferences".to_owned(),
            format!(
                "{} Shadowroom keeps retrying.",
                cause.map_or_else(|| sentence(last), str::to_owned)
            ),
        ),
        NoticeKind::General => {
            if let Some((title, rest)) = parts
                .split_first()
                .filter(|(_, rest)| !rest.is_empty())
                .and_then(|(first, rest)| area_title(first).map(|title| (title, rest)))
            {
                let detail = cause.map_or_else(|| sentence(&rest.join(": ")), str::to_owned);
                return ToastCopy {
                    title,
                    detail: Some(detail),
                    raw: lossy.then(|| text.to_owned()),
                };
            }
            // "Headline. Explanation." keeps its first sentence as the title.
            match text.split_once(". ") {
                Some((first, rest)) if cause.is_none() && !rest.trim().is_empty() => {
                    (first.to_owned(), rest.trim().to_owned())
                }
                _ => {
                    return ToastCopy {
                        title: if cause.is_some() {
                            parts.first().copied().unwrap_or(text).to_owned()
                        } else {
                            text.to_owned()
                        },
                        detail: cause.map(str::to_owned),
                        raw: cause.is_some().then(|| text.to_owned()),
                    };
                }
            }
        }
    };
    ToastCopy {
        title,
        detail: Some(detail),
        raw: lossy.then(|| text.to_owned()),
    }
}

/// Capitalises a fragment and ends it with a full stop.
fn sentence(fragment: &str) -> String {
    let fragment = fragment.trim();
    let mut chars = fragment.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out: String = first.to_uppercase().chain(chars).collect();
    if !out.ends_with(['.', '!', '?', '…']) {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plurals_agree_with_their_count() {
        assert_eq!(plural(1, "photo", "photos"), "1 photo");
        assert_eq!(plural(0, "photo", "photos"), "0 photos");
        assert_eq!(
            plural(2, "duplicate set", "duplicate sets"),
            "2 duplicate sets"
        );
        assert_eq!(plural(1, "file", "files"), "1 file");
    }

    #[test]
    fn batch_decisions_ask_and_report_with_counts() {
        assert_eq!(
            batch_question(None, Some(Flag::Reject), None, 1_200),
            "Reject 1,200 photos?"
        );
        assert_eq!(
            batch_done(None, Some(Flag::Reject), None, 1_200),
            "Rejected 1,200 photos"
        );
        assert_eq!(batch_done(Some(3), None, None, 60), "Rated 60 photos ★★★");
        assert_eq!(
            batch_done(None, None, Some(ColorLabel::Red), 2),
            "Labelled 2 photos Red"
        );
        assert_eq!(
            batch_question(Some(0), Some(Flag::None), None, 51),
            "Reset 51 photos?"
        );
    }

    #[test]
    fn grouping_gaps_read_in_units() {
        assert_eq!(duration(500), "0.5 s");
        assert_eq!(duration(2_000), "2 s");
        assert_eq!(duration(3_600_000), "60 min");
    }

    #[test]
    fn large_counts_are_grouped() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(50_112), "50,112");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(plural(50_112, "photo", "photos"), "50,112 photos");
    }

    #[test]
    fn rating_thresholds_never_say_one_stars() {
        assert_eq!(rating_threshold(0), "All ratings");
        assert_eq!(rating_threshold(1), "1 star and up");
        assert_eq!(rating_threshold(3), "3 stars and up");
        assert_eq!(rating_threshold(5), "5 stars");
    }

    #[test]
    fn decision_summaries_name_flag_stars_and_label() {
        assert_eq!(
            decision_summary(Flag::Pick, 5, ColorLabel::None),
            "Pick · ★★★★★"
        );
        assert_eq!(
            decision_summary(Flag::Reject, 0, ColorLabel::Red),
            "Reject · Red"
        );
        assert_eq!(decision_summary(Flag::None, 2, ColorLabel::None), "★★");
        assert_eq!(decision_summary(Flag::None, 0, ColorLabel::None), "Cleared");
        assert_eq!(subject(1, Some("02-lakeside.jpg")), "02-lakeside.jpg");
        assert_eq!(subject(3, Some("02-lakeside.jpg")), "3 photos");
        assert_eq!(subject(1, None), "1 photo");
    }

    #[test]
    fn sizes_and_import_labels_read_naturally() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(999), "999 B");
        assert_eq!(fmt_bytes(2_400_000), "2.4 MB");
        assert_eq!(fmt_bytes(48_200_000_000), "48 GB");
        assert_eq!(fmt_bytes(1_000), "1.0 KB");
        assert_eq!(import_label(1, 0, 2_400_000), "Import 1 photo (2.4 MB)");
        assert_eq!(
            import_label(612, 3, 48_200_000_000),
            "Import 612 photos · 3 videos (48 GB)"
        );
    }

    #[test]
    fn duplicate_empty_states_follow_the_scan() {
        let (title, body, button) = duplicates_empty(None, "in Demo Shoot");
        assert_eq!(title, "Review duplicate photos");
        assert!(body.contains("in Demo Shoot"));
        assert_eq!(button, Some("Find Duplicates"));
        let mut scan = DuplicateScanState {
            phase: ScanPhase::Completed,
            total: 7,
            ..DuplicateScanState::default()
        };
        let (title, body, button) = duplicates_empty(Some(&scan), "in the whole library");
        assert_eq!(title, "No exact duplicates");
        assert!(body.starts_with("Checked 7 photos in the whole library"));
        assert_eq!(button, Some("Scan again"));
        scan.phase = ScanPhase::Running;
        scan.processed = 3;
        scan.candidates = 4;
        let (title, body, button) = duplicates_empty(Some(&scan), "in X");
        assert_eq!(title, "Checking for duplicates…");
        assert!(body.contains("3 of 4"));
        assert_eq!(button, None);
        scan.phase = ScanPhase::Stale;
        assert_eq!(
            duplicates_empty(Some(&scan), "").0,
            "Duplicate results are out of date"
        );
        scan.phase = ScanPhase::Failed;
        scan.errors = vec!["Permission denied".into()];
        let (title, body, button) = duplicates_empty(Some(&scan), "");
        assert_eq!(title, "Duplicate scan failed");
        assert_eq!(body, "Permission denied");
        assert_eq!(button, Some("Retry"));
    }

    #[test]
    fn locked_catalog_reads_as_plain_language() {
        let raw = "Decisions remain unsaved: Decision was not saved: database is locked: \
                   Error code 5: The database file is locked";
        let copy = toast_copy(NoticeKind::UnsavedDecisions, raw, 1);
        assert_eq!(copy.title, "Couldn't save 1 decision");
        assert_eq!(
            copy.detail.as_deref(),
            Some("The catalog is busy (database locked). Your changes are kept in this window.")
        );
        assert!(!copy.title.contains("Error code") && !copy.detail.unwrap().contains("code"));
        assert_eq!(copy.raw.as_deref(), Some(raw));
        let copy = toast_copy(NoticeKind::UnsavedDecisions, raw, 3);
        assert_eq!(copy.title, "Couldn't save 3 decisions");
    }

    #[test]
    fn preference_failures_say_they_retry() {
        let copy = toast_copy(
            NoticeKind::UnsavedPreferences,
            "Preferences remain unsaved: Permission denied (os error 13)",
            0,
        );
        assert_eq!(copy.title, "Couldn't save preferences");
        assert_eq!(
            copy.detail.as_deref(),
            Some("Permission was denied. Shadowroom keeps retrying.")
        );
    }

    #[test]
    fn area_prefixes_become_headlines() {
        let copy = toast_copy(
            NoticeKind::General,
            "Import: /media/card/IMG_0412.CR3: No such file or directory (os error 2)",
            0,
        );
        assert_eq!(copy.title, "Import failed");
        assert_eq!(copy.detail.as_deref(), Some("A file or folder is missing."));
        assert!(copy.raw.is_some_and(|raw| raw.contains("IMG_0412")));
        let copy = toast_copy(NoticeKind::General, "Export: destination is not empty", 0);
        assert_eq!(copy.title, "Export failed");
        assert_eq!(copy.detail.as_deref(), Some("Destination is not empty."));
        assert_eq!(copy.raw, None);
    }

    #[test]
    fn sentences_split_into_title_and_detail() {
        let copy = toast_copy(
            NoticeKind::General,
            "Decision writer is disconnected. Your changes remain unsaved in this window.",
            0,
        );
        assert_eq!(copy.title, "Decision writer is disconnected");
        assert_eq!(
            copy.detail.as_deref(),
            Some("Your changes remain unsaved in this window.")
        );
        let copy = toast_copy(NoticeKind::General, "Select photos before deleting", 0);
        assert_eq!(copy.title, "Select photos before deleting");
        assert_eq!(copy.detail, None);
        assert_eq!(copy.raw, None);
    }

    #[test]
    fn engine_codes_are_recognised_only_as_whole_segments() {
        assert!(is_code("Error code 5"));
        assert!(is_code("error code 1555"));
        assert!(!is_code("Error code"));
        assert!(!is_code("Error codes differ"));
        assert_eq!(segments("a: Error code 5: b"), ["a", "b"]);
    }
}
