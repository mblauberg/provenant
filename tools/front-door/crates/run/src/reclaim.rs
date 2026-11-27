//! Deadline-aware interface for resumable cleanup work.
//!
//! One owner (a retention class) is driven at a time. The owner decides which
//! candidates are ranked and what applying them means; this module owns the
//! deadline checks, the page cursor and the rule that a resume finishes the
//! open page before it advances. A cursor is persisted by the caller so a
//! restart never loses a candidate that was ranked but not yet applied.

use provenant_core::Deadline;
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

/// A bounded cleanup owner. Implementations keep their own candidate policy;
/// the shared driver owns deadline checks and cursor persistence.
#[allow(dead_code)]
pub trait Reclaimable {
    type Plan;
    type Error;

    fn class(&self) -> &'static str;

    fn scan(
        &mut self,
        cursor: Option<&ReclaimCursor>,
        deadline: &Deadline,
    ) -> Result<(Self::Plan, ReclaimReport), Self::Error>;

    fn apply(
        &mut self,
        plan: Self::Plan,
        deadline: &Deadline,
    ) -> Result<ReclaimReport, Self::Error>;
}

/// Drive one owner for a bounded pass. `save` receives the cursor to persist;
/// `None` means the owner is complete and any saved cursor can be cleared.
/// Scan and apply share the one deadline, so time spent selecting candidates
/// is charged against the same budget as applying them.
pub fn drive<R: Reclaimable>(
    owner: &mut R,
    cursor: Option<&ReclaimCursor>,
    deadline: &Deadline,
    mut save: impl FnMut(Option<&ReclaimCursor>),
) -> Result<ReclaimReport, R::Error> {
    let (plan, mut report) = owner.scan(cursor, deadline)?;
    report.merge(owner.apply(plan, deadline)?);
    if report.complete {
        save(None);
    } else {
        save(report.cursor.as_ref());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime};

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

    struct FakeOwner {
        pages: usize,
    }

    impl Reclaimable for FakeOwner {
        type Plan = u64;
        type Error = String;

        fn class(&self) -> &'static str {
            "Fixture"
        }

        fn scan(
            &mut self,
            cursor: Option<&ReclaimCursor>,
            _deadline: &Deadline,
        ) -> Result<(Self::Plan, ReclaimReport), Self::Error> {
            let seq = cursor.map_or(0, |cursor| cursor.page_end_seq);
            if seq >= self.pages {
                return Ok((
                    seq,
                    ReclaimReport {
                        complete: true,
                        ..ReclaimReport::default()
                    },
                ));
            }
            Ok((seq, ReclaimReport::default()))
        }

        fn apply(
            &mut self,
            plan: Self::Plan,
            _deadline: &Deadline,
        ) -> Result<ReclaimReport, Self::Error> {
            Ok(ReclaimReport {
                scanned: 1,
                reclaimed: 1,
                complete: plan + 1 >= self.pages,
                cursor: Some(ReclaimCursor::next_page(plan, 1)),
            })
        }
    }

    #[test]
    fn drive_merges_scan_and_apply_and_persists_the_cursor() {
        let deadline = Deadline::at(MonoTime::from_micros(10_000));
        let mut owner = FakeOwner { pages: 3 };
        let mut saved: Option<ReclaimCursor> = None;
        let mut final_report = None;
        for _ in 0..2 {
            final_report = Some(
                drive(&mut owner, saved.as_ref(), &deadline, |cursor| {
                    saved = cursor.cloned();
                })
                .unwrap(),
            );
        }
        let report = final_report.unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.reclaimed, 1);
        assert!(!report.complete);
        assert_eq!(saved, Some(ReclaimCursor::next_page(1, 1)));

        let deadline = Deadline::after(
            MonoTime::from_micros(10_000),
            DurationMicros::from_micros(100),
        )
        .unwrap();
        let report = drive(&mut owner, saved.as_ref(), &deadline, |cursor| {
            assert!(cursor.is_none(), "a complete owner clears its cursor");
        })
        .unwrap();
        assert!(report.complete);
    }
}
