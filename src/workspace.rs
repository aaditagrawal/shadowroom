//! Small, platform-independent workspace rules shared by the desktop and tests.
use crate::model::{ColorLabel, Flag, Photo};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Revision identity, rather than value equality, determines which write an
/// acknowledgement commits. Repeated values (5 → 2 → 5) remain distinct writes.
#[derive(Default)]
pub struct SaveRevisions {
    revision: u64,
    latest: HashMap<i64, u64>,
    pending: VecDeque<(u64, Vec<i64>)>,
}
impl SaveRevisions {
    pub fn stage(&mut self, ids: impl IntoIterator<Item = i64>) -> u64 {
        self.revision += 1;
        let ids: Vec<_> = ids.into_iter().collect();
        for id in &ids {
            self.latest.insert(*id, self.revision);
        }
        self.pending.push_back((self.revision, ids));
        self.revision
    }
    pub fn acknowledge(&mut self, success: bool) -> Vec<i64> {
        let Some((revision, ids)) = self.pending.pop_front() else {
            return Vec::new();
        };
        if !success {
            return Vec::new();
        }
        // Reuse the queued vector, and probe the revision table once per ID.
        let mut ids = ids;
        ids.retain(|id| {
            if let std::collections::hash_map::Entry::Occupied(entry) = self.latest.entry(*id) {
                if *entry.get() == revision {
                    entry.remove();
                    return true;
                }
            }
            false
        });
        ids
    }
    pub fn disconnect(&mut self) {
        self.pending.clear();
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Picks,
    Unrated,
    Rejects,
    Trash,
}
impl Filter {
    pub fn accepts(self, photo: &Photo) -> bool {
        if self == Self::Trash {
            return photo.trashed;
        }
        if photo.trashed {
            return false;
        }
        match self {
            Self::Trash => unreachable!(),
            Self::All => true,
            Self::Picks => photo.flag == Flag::Pick,
            Self::Unrated => photo.rating == 0 && photo.flag == Flag::None,
            Self::Rejects => photo.flag == Flag::Reject,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::All => "All photographs",
            Self::Picks => "Selected",
            Self::Unrated => "Unreviewed",
            Self::Rejects => "Rejected",
            Self::Trash => "Trash",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: i64,
    pub rating: u8,
    pub flag: Flag,
    pub label: ColorLabel,
    #[serde(default)]
    pub trashed: bool,
}
impl From<&Photo> for Decision {
    fn from(p: &Photo) -> Self {
        Self {
            id: p.id,
            rating: p.rating,
            flag: p.flag,
            label: p.label,
            trashed: p.trashed,
        }
    }
}
pub struct History {
    undo: VecDeque<(Vec<Decision>, Vec<Decision>)>,
    redo: Vec<(Vec<Decision>, Vec<Decision>)>,
    snapshot_bytes: usize,
    byte_budget: usize,
}
impl Default for History {
    fn default() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            snapshot_bytes: 0,
            byte_budget: 64 * 1024 * 1024,
        }
    }
}
fn snapshot_bytes(item: &(Vec<Decision>, Vec<Decision>)) -> usize {
    item.0
        .capacity()
        .saturating_add(item.1.capacity())
        .saturating_mul(std::mem::size_of::<Decision>())
}
impl History {
    pub fn push(&mut self, before: Vec<Decision>, after: Vec<Decision>) {
        if before == after {
            return;
        }
        self.snapshot_bytes = self
            .snapshot_bytes
            .saturating_sub(self.redo.iter().map(snapshot_bytes).sum::<usize>());
        self.redo.clear();
        let item = (before, after);
        self.snapshot_bytes = self.snapshot_bytes.saturating_add(snapshot_bytes(&item));
        self.undo.push_back(item);
        // Keep complete commands and at least the latest edit, even when one
        // exceptionally large selection exceeds the snapshot budget by itself.
        while self.undo.len() > 1
            && (self.undo.len() > 1000 || self.snapshot_bytes > self.byte_budget)
        {
            let evicted = self.undo.pop_front().unwrap();
            self.snapshot_bytes = self.snapshot_bytes.saturating_sub(snapshot_bytes(&evicted));
        }
    }
    pub fn undo(&mut self) -> Option<Vec<Decision>> {
        let item = self.undo.pop_back()?;
        let result = item.0.clone();
        self.redo.push(item);
        Some(result)
    }
    pub fn redo(&mut self) -> Option<Vec<Decision>> {
        let item = self.redo.pop()?;
        let result = item.1.clone();
        self.undo.push_back(item);
        Some(result)
    }
}

/// Ordered-subsequence matching rewards word starts and contiguous letters.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_ascii() && text.is_ascii() {
        if query.is_empty() {
            return Some(0);
        }
        let mut chars = query
            .bytes()
            .filter(|c| !char::from(*c).is_whitespace())
            .peekable();
        let mut score = 0;
        let mut previous = false;
        let mut boundary = true;
        for c in text.bytes() {
            if chars.peek().is_some_and(|q| q.eq_ignore_ascii_case(&c)) {
                chars.next();
                score += if previous {
                    8
                } else if boundary {
                    6
                } else {
                    1
                };
                previous = true;
            } else {
                previous = false;
                score -= 1;
            }
            boundary = !c.is_ascii_alphanumeric();
            if chars.peek().is_none() {
                return Some(score);
            }
        }
        return None;
    }
    // Full string lowercasing preserves Unicode's contextual mappings (e.g. Σ).
    let query = query.to_lowercase();
    let text = text.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let mut chars = query.chars().filter(|c| !c.is_whitespace()).peekable();
    let mut score = 0;
    let mut previous = false;
    let mut boundary = true;
    for c in text.chars() {
        if Some(&c) == chars.peek() {
            chars.next();
            score += if previous {
                8
            } else if boundary {
                6
            } else {
                1
            };
            previous = true;
        } else {
            previous = false;
            score -= 1;
        }
        boundary = !c.is_alphanumeric();
        if chars.peek().is_none() {
            return Some(score);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn history_change(id: i64, count: usize) -> (Vec<Decision>, Vec<Decision>) {
        let before = vec![
            Decision {
                id,
                rating: 0,
                flag: Flag::None,
                label: ColorLabel::None,
                trashed: false
            };
            count
        ];
        let after = before
            .iter()
            .map(|d| Decision { rating: 5, ..*d })
            .collect();
        (before, after)
    }
    #[test]
    fn history_byte_budget_keeps_whole_commands_and_accounts_for_redo() {
        let cost = 6 * std::mem::size_of::<Decision>();
        let mut history = History {
            byte_budget: 2 * cost,
            ..History::default()
        };
        for id in 1..=4 {
            let (before, after) = history_change(id, 3);
            history.push(before, after);
        }
        assert_eq!(history.snapshot_bytes, 2 * cost);
        assert_eq!(history.undo().unwrap(), history_change(4, 3).0);
        assert_eq!(history.undo().unwrap(), history_change(3, 3).0);
        assert!(history.undo().is_none());
        assert_eq!(history.snapshot_bytes, 2 * cost);
        assert_eq!(history.redo().unwrap(), history_change(3, 3).1);
        let (before, after) = history_change(5, 3);
        history.push(before, after);
        assert_eq!(history.snapshot_bytes, 2 * cost);
        assert!(history.redo().is_none());
        assert_eq!(history.undo().unwrap()[0].id, 5);
        assert_eq!(history.undo().unwrap()[0].id, 3);
    }
    #[test]
    fn oversized_latest_edit_remains_undoable_and_is_released_by_a_new_branch() {
        let mut history = History {
            byte_budget: 128,
            ..History::default()
        };
        for id in 1..=2 {
            let (before, after) = history_change(id, 1);
            history.push(before, after);
        }
        let (before, after) = history_change(3, 128);
        history.push(before, after);
        assert_eq!(history.undo.len(), 1);
        assert!(history.snapshot_bytes > history.byte_budget);
        let restored = history.undo().unwrap();
        assert_eq!(restored.len(), 128);
        assert_eq!(history.redo.len(), 1);
        let (before, after) = history_change(4, 1);
        history.push(before, after);
        assert!(history.redo.is_empty());
        assert!(history.snapshot_bytes <= history.byte_budget);
    }
    fn reference_fuzzy(query: &str, text: &str) -> Option<i32> {
        let query = query.to_lowercase();
        let text = text.to_lowercase();
        if query.is_empty() {
            return Some(0);
        }
        let mut chars = query.chars().filter(|c| !c.is_whitespace()).peekable();
        let mut score = 0;
        let mut previous = false;
        let mut boundary = true;
        for c in text.chars() {
            if Some(&c) == chars.peek() {
                chars.next();
                score += if previous {
                    8
                } else if boundary {
                    6
                } else {
                    1
                };
                previous = true;
            } else {
                previous = false;
                score -= 1;
            }
            boundary = !c.is_alphanumeric();
            if chars.peek().is_none() {
                return Some(score);
            }
        }
        None
    }
    #[test]
    fn fuzzy_fast_path_preserves_original_scores() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(437);
        for _ in 0..10_000 {
            let text: String = (0..rng.random_range(0..70))
                .map(|_| char::from(rng.random_range(0..128)))
                .collect();
            let query: String = (0..rng.random_range(0..8))
                .map(|_| char::from(rng.random_range(0..128)))
                .collect();
            assert_eq!(fuzzy_score(&query, &text), reference_fuzzy(&query, &text));
        }
        for query in ["", " ", "\t", "thè", "ΟΣ", "i\u{307}", "σ", "ς"] {
            for text in ["", " ", "Thème", "ΟΣ", "İstanbul", "Copy Photo"] {
                assert_eq!(fuzzy_score(query, text), reference_fuzzy(query, text));
            }
        }
    }
    #[test]
    fn history_keeps_exactly_the_last_thousand_entries() {
        let mut history = History::default();
        for id in 0..1002 {
            let a = Decision {
                id,
                rating: 0,
                flag: Flag::None,
                label: ColorLabel::None,
                trashed: false,
            };
            history.push(vec![a], vec![Decision { rating: 1, ..a }]);
        }
        for id in (2..1002).rev() {
            assert_eq!(history.undo().unwrap()[0].id, id);
        }
        assert!(history.undo().is_none());
        for id in 2..1002 {
            assert_eq!(history.redo().unwrap()[0].id, id);
        }
        assert!(history.redo().is_none());
    }
    #[test]
    fn duplicate_save_ids_acknowledge_only_once() {
        let mut revisions = SaveRevisions::default();
        revisions.stage([1, 1, 2]);
        assert_eq!(revisions.acknowledge(true), vec![1, 2]);
    }
    #[test]
    #[ignore]
    fn bench_workspace() {
        let began = std::time::Instant::now();
        for _ in 0..10_000 {
            for _ in 0..100 {
                std::hint::black_box(fuzzy_score(
                    std::hint::black_box("cps"),
                    std::hint::black_box("Copy selected photographs to folder"),
                ));
            }
        }
        eprintln!("palette 100 commands mean {:?}", began.elapsed() / 10_000);
        let mut history = History::default();
        let a = Decision {
            id: 1,
            rating: 0,
            flag: Flag::None,
            label: ColorLabel::None,
            trashed: false,
        };
        let began = std::time::Instant::now();
        for _ in 0..100_000 {
            history.push(vec![a], vec![Decision { rating: 1, ..a }]);
        }
        eprintln!("history 100k pushes {:?}", began.elapsed());
    }
    #[test]
    fn repeated_values_do_not_acknowledge_newer_writes() {
        let mut saves = SaveRevisions::default();
        saves.stage([1]);
        saves.stage([1]);
        saves.stage([1]);
        assert!(saves.acknowledge(true).is_empty());
        assert!(saves.acknowledge(true).is_empty());
        assert_eq!(saves.acknowledge(true), vec![1]);
    }
    #[test]
    fn failed_batch_stays_dirty_and_retry_has_a_new_revision() {
        let mut saves = SaveRevisions::default();
        saves.stage([1, 2]);
        assert!(saves.acknowledge(false).is_empty());
        saves.stage([1]);
        assert_eq!(saves.acknowledge(true), vec![1]);
        saves.stage([2]);
        saves.disconnect();
        assert!(saves.acknowledge(true).is_empty());
        assert_eq!(saves.stage([2]), 4);
        assert_eq!(saves.acknowledge(true), vec![2]);
    }
    #[test]
    fn history_restores_full_decision_and_invalidates_redo() {
        let a = Decision {
            id: 1,
            rating: 0,
            flag: Flag::None,
            label: ColorLabel::Blue,
            trashed: false,
        };
        let b = Decision {
            rating: 5,
            flag: Flag::Pick,
            trashed: true,
            ..a
        };
        let mut h = History::default();
        h.push(vec![a], vec![b]);
        assert_eq!(h.undo(), Some(vec![a]));
        assert_eq!(h.redo(), Some(vec![b]));
        h.undo();
        h.push(vec![a], vec![Decision { rating: 2, ..a }]);
        assert_eq!(h.redo(), None);
    }
    #[test]
    fn fuzzy_search_handles_words_and_unicode() {
        assert!(fuzzy_score("cp", "Copy selected photographs").is_some());
        assert!(fuzzy_score("xyz", "Copy selected photographs").is_none());
        assert!(fuzzy_score("thè", "Thème").is_some());
    }
    #[test]
    fn unreviewed_requires_no_rating_and_no_flag() {
        let mut p = Photo::default();
        assert!(Filter::Unrated.accepts(&p));
        p.flag = Flag::Reject;
        assert!(!Filter::Unrated.accepts(&p));
        p.flag = Flag::None;
        p.rating = 1;
        assert!(!Filter::Unrated.accepts(&p));
    }
    #[test]
    fn trash_is_restorable_and_excluded_from_every_live_filter() {
        let mut photo = Photo {
            trashed: true,
            flag: Flag::Pick,
            ..Default::default()
        };
        for filter in [Filter::All, Filter::Picks, Filter::Unrated, Filter::Rejects] {
            assert!(!filter.accepts(&photo));
        }
        assert!(Filter::Trash.accepts(&photo));
        photo.trashed = false;
        assert!(Filter::Picks.accepts(&photo));
        assert!(!Filter::Trash.accepts(&photo));
        let old = r#"{"id":1,"rating":3,"flag":"Pick","label":"Blue"}"#;
        assert!(!serde_json::from_str::<Decision>(old).unwrap().trashed);
        assert!(
            !serde_json::from_str::<crate::model::PhotoDecision>(old)
                .unwrap()
                .trashed
        );
    }
}
