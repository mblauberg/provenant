//! Page cursor and report for resumable cleanup work.
//!
//! This module owns the page cursor and the rule that a resume finishes the
//! open page before it advances. A cursor is persisted by the caller so a
//! restart never loses a candidate that was ranked but not yet applied.

use serde::{Deserialize, Serialize};

/// Where a resumable sweep stopped. Candidates are ranked by a stable sequence
/// number; a page is a half-open `[start, end)` window of that ranking.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimCursor {
    pub page_start_seq: u64,
    pub page_end_seq: u64,
    /// Stable candidate identities applied in the current page. A candidate is
    /// added only once its effect has been applied, so a resume returns to
    /// every ranked candidate that was still outstanding when the budget hit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visited: Vec<String>,
}

impl ReclaimCursor {
    /// A page is open when it can still hold unvisited candidates.
    pub fn page_open(&self) -> bool {
        self.page_end_seq > self.page_start_seq
    }

    /// Open the page starting at `after_seq`, covering `size` ranked slots.
    /// A zero or absent size still advances by one slot, so progress never
    /// stalls.
    pub fn next_page(after_seq: u64, size: u64) -> Self {
        let width = size.max(1);
        Self {
            page_start_seq: after_seq,
            page_end_seq: after_seq.saturating_add(width),
            visited: Vec::new(),
        }
    }

    /// Whether a ranked sequence belongs to the open page.
    pub fn in_page(&self, seq: u64) -> bool {
        self.page_open() && seq >= self.page_start_seq && seq < self.page_end_seq
    }

    /// Whether this identity was already applied in the open page.
    pub fn is_visited(&self, id: &str) -> bool {
        self.visited.iter().any(|visited| visited == id)
    }

    /// Mark one applied candidate as visited in the open page.
    pub fn mark_visited(&mut self, id: impl Into<String>) {
        let id = id.into();
        if !self.is_visited(&id) {
            self.visited.push(id);
        }
    }

    /// The page is complete once every one of its slots has been applied.
    pub fn page_complete(&self) -> bool {
        if !self.page_open() {
            return true;
        }
        let slots = self.page_end_seq.saturating_sub(self.page_start_seq) as usize;
        self.visited.len() >= slots
    }

    /// Close a complete page so the next plan opens the following one. A page
    /// that is not complete is left untouched: a resume must finish it first.
    pub fn close_page_if_complete(&mut self) {
        if self.page_complete() {
            self.page_start_seq = self.page_end_seq;
            self.visited.clear();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimReport {
    pub scanned: usize,
    pub reclaimed: usize,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ReclaimCursor>,
}

impl ReclaimReport {
    /// Fold an apply report into a scan report. Scanned and reclaimed counts
    /// accumulate; the later cursor and completion state win.
    pub fn merge(&mut self, other: Self) {
        self.scanned = self.scanned.saturating_add(other.scanned);
        self.reclaimed = self.reclaimed.saturating_add(other.reclaimed);
        self.complete = other.complete;
        if other.cursor.is_some() {
            self.cursor = other.cursor;
        }
    }
}

/// The candidates selected for one bounded pass, with the cursor to persist
/// after they have been applied. Selection does not mark a candidate visited:
/// a budget can stop between selection and application.
#[derive(Debug, PartialEq, Eq)]
pub struct ReclaimPage<'a> {
    pub cursor: ReclaimCursor,
    pub candidates: Vec<(u64, &'a str)>,
}

/// Select the candidates a pass should apply.
///
/// An open page always wins: returning to it yields the candidates ranked into
/// that page but not yet applied. Only once the page is complete does the next
/// call open the following page. This is what makes a resume visit every
/// ranked candidate exactly once even when the budget interrupts a page part
/// way through (#4099).
pub fn plan_page<'a, I>(saved: Option<&ReclaimCursor>, ranked: I, page_size: u64) -> ReclaimPage<'a>
where
    I: IntoIterator<Item = (u64, &'a str)>,
{
    let cursor = saved
        .filter(|cursor| cursor.page_open())
        .cloned()
        .unwrap_or_else(|| {
            let last = saved.map_or(0, |cursor| cursor.page_end_seq);
            ReclaimCursor::next_page(last, page_size)
        });
    let candidates = ranked
        .into_iter()
        .filter(|(seq, id)| cursor.in_page(*seq) && !cursor.is_visited(id))
        .collect::<Vec<_>>();
    ReclaimPage { cursor, candidates }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> [&'static str; 7] {
        ["a", "b", "c", "d", "e", "f", "g"]
    }

    fn stream() -> Vec<(u64, &'static str)> {
        candidates()
            .into_iter()
            .enumerate()
            .map(|(index, id)| (index as u64, id))
            .collect()
    }

    #[test]
    fn plan_page_opens_the_first_page_in_rank_order() {
        let page = plan_page(None, stream(), 3);
        assert_eq!(page.cursor.page_start_seq, 0);
        assert_eq!(page.cursor.page_end_seq, 3);
        assert_eq!(page.candidates, vec![(0, "a"), (1, "b"), (2, "c")]);
        assert!(page.cursor.visited.is_empty());
    }

    #[test]
    fn resume_finishes_the_open_page_before_advancing() {
        // The first pass applied only "a" and "b" before the budget stopped it;
        // its cursor still names the unfinished first page. "c" was selected
        // but not applied, so it must be offered again.
        let mut saved = ReclaimCursor::next_page(0, 3);
        saved.mark_visited("a");
        saved.mark_visited("b");
        let resume = plan_page(Some(&saved), stream(), 3);
        assert_eq!(resume.cursor.page_start_seq, 0);
        assert_eq!(resume.cursor.page_end_seq, 3);
        assert_eq!(resume.candidates, vec![(2, "c")]);

        // With the whole page visited, the next plan opens page two rather than
        // revisiting any candidate from page one.
        saved.mark_visited("c");
        saved.close_page_if_complete();
        assert_eq!(saved.page_start_seq, 3);
        let next = plan_page(Some(&saved), stream(), 3);
        assert_eq!(next.cursor.page_start_seq, 3);
        assert_eq!(next.cursor.page_end_seq, 6);
        assert_eq!(next.candidates, vec![(3, "d"), (4, "e"), (5, "f")]);
    }

    #[test]
    fn gc_resume_visits_every_ranked_candidate() {
        let mut saved: Option<ReclaimCursor> = None;
        let mut applied = Vec::new();
        loop {
            let page = plan_page(saved.as_ref(), stream(), 2);
            if page.candidates.is_empty() {
                break;
            }
            // Only the first candidate of each page is applied before the
            // budget stops this pass. The rest belong to the same open page
            // and must be offered to the next pass.
            let mut cursor = page.cursor;
            cursor.mark_visited(page.candidates[0].1);
            applied.push(page.candidates[0].1);
            cursor.close_page_if_complete();
            saved = Some(cursor);
        }
        assert_eq!(applied, ["a", "b", "c", "d", "e", "f", "g"]);
        assert_eq!(saved.map(|cursor| cursor.page_end_seq), Some(8));
    }
}
