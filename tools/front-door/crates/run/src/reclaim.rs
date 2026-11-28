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
    /// A page is open while it still names a non-empty sequence range.
    pub fn page_open(&self) -> bool {
        self.page_end_seq > self.page_start_seq
    }

    /// Open a page containing the candidates between its first and last
    /// sequence, inclusive. The persisted end is exclusive.
    fn covering(first_seq: u64, last_seq: u64) -> Self {
        Self {
            page_start_seq: first_seq,
            page_end_seq: last_seq.saturating_add(1),
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

    /// Close a page after the caller has applied every candidate returned for
    /// it. The caller, which owns the candidate scan, is the only component
    /// that can know whether a page is complete.
    pub fn close_page(&mut self) {
        self.page_start_seq = self.page_end_seq;
        self.visited.clear();
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

/// One page prepared by a [`Reclaimable`] class.
#[derive(Debug)]
pub struct ReclaimPlan {
    pub indices: Vec<usize>,
}

/// The production scan/apply boundary for one reclaimable class.
///
/// `scan` owns page selection and persists the open cursor in the class
/// adapter. `apply` marks a candidate visited only after its effect succeeds;
/// a budget stop or failed effect therefore leaves it available to a resume.
pub trait Reclaimable {
    type Candidate;
    type Error;

    fn class(&self) -> &'static str;
    fn scan(
        &mut self,
        cursor: Option<&ReclaimCursor>,
        ranked: &[Self::Candidate],
        deadline: &dyn Fn() -> bool,
        page_size: u64,
    ) -> Result<Option<ReclaimPlan>, Self::Error>;
    fn apply<F>(
        &mut self,
        candidate: &Self::Candidate,
        deadline: &dyn Fn() -> bool,
        effect: F,
    ) -> Result<bool, Self::Error>
    where
        F: FnOnce() -> Result<(), Self::Error>;
    fn cursor(&self) -> Option<&ReclaimCursor>;
    fn finish_page(&mut self);
}

/// Adapter that gives a sequence-ranked collection the shared scan/apply
/// semantics used by scratch and provider-state retention.
pub struct ReclaimDriver<Candidate, Error> {
    class: &'static str,
    sequence: fn(&Candidate) -> u64,
    identity: fn(&Candidate) -> &str,
    cursor: Option<ReclaimCursor>,
    _error: std::marker::PhantomData<fn() -> Error>,
}

impl<Candidate, Error> ReclaimDriver<Candidate, Error> {
    pub fn new(
        class: &'static str,
        sequence: fn(&Candidate) -> u64,
        identity: fn(&Candidate) -> &str,
        cursor: Option<ReclaimCursor>,
    ) -> Self {
        Self {
            class,
            sequence,
            identity,
            cursor,
            _error: std::marker::PhantomData,
        }
    }
}

impl<Candidate, Error> Reclaimable for ReclaimDriver<Candidate, Error>
where
    Candidate: Clone,
{
    type Candidate = Candidate;
    type Error = Error;

    fn class(&self) -> &'static str {
        self.class
    }

    fn scan(
        &mut self,
        cursor: Option<&ReclaimCursor>,
        ranked: &[Self::Candidate],
        deadline: &dyn Fn() -> bool,
        page_size: u64,
    ) -> Result<Option<ReclaimPlan>, Self::Error> {
        if !deadline() {
            return Ok(None);
        }
        let page = plan_page(
            cursor,
            ranked
                .iter()
                .map(|candidate| ((self.sequence)(candidate), (self.identity)(candidate))),
            page_size,
        );
        let selected = page
            .candidates
            .iter()
            .map(|(_, id)| (*id).to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        self.cursor = Some(page.cursor);
        let indices = ranked
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                selected
                    .contains((self.identity)(candidate))
                    .then_some(index)
            })
            .collect();
        Ok(Some(ReclaimPlan { indices }))
    }

    fn apply<F>(
        &mut self,
        candidate: &Self::Candidate,
        deadline: &dyn Fn() -> bool,
        effect: F,
    ) -> Result<bool, Self::Error>
    where
        F: FnOnce() -> Result<(), Self::Error>,
    {
        if !deadline() {
            return Ok(false);
        }
        effect()?;
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.mark_visited((self.identity)(candidate));
        }
        Ok(true)
    }

    fn cursor(&self) -> Option<&ReclaimCursor> {
        self.cursor.as_ref()
    }

    fn finish_page(&mut self) {
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.close_page();
        }
    }
}

/// Select the candidates a pass should apply from a sequence-ordered scan.
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
    let ranked = ranked.into_iter().collect::<Vec<_>>();
    let cursor = if let Some(cursor) = saved.filter(|cursor| cursor.page_open()) {
        cursor.clone()
    } else {
        let after = saved.map_or(0, |cursor| cursor.page_end_seq);
        let mut sequences = ranked
            .iter()
            .map(|(sequence, _)| *sequence)
            .filter(|sequence| *sequence >= after)
            .collect::<Vec<_>>();
        sequences.sort_unstable();
        let count = usize::try_from(page_size.max(1)).unwrap_or(usize::MAX);
        match sequences.get(..sequences.len().min(count)) {
            Some(selected) if !selected.is_empty() => {
                ReclaimCursor::covering(selected[0], selected[selected.len() - 1])
            }
            _ => ReclaimCursor {
                page_start_seq: after,
                page_end_seq: after,
                visited: Vec::new(),
            },
        }
    };
    let candidates = ranked
        .into_iter()
        .filter(|(seq, id)| cursor.in_page(*seq) && !cursor.is_visited(id))
        .collect::<Vec<_>>();
    ReclaimPage { cursor, candidates }
}

/// Store a reclaim cursor in the existing GC continuation detail field.
pub fn encode_cursor(class: &str, cursor: &ReclaimCursor) -> Result<String, serde_json::Error> {
    serde_json::to_string(cursor).map(|serialized| format!("reclaim:{class}:{serialized}"))
}

/// Read a cursor saved by [`encode_cursor`]. A malformed reclaim continuation
/// is reported to the caller instead of silently resuming after an attempt.
pub fn decode_cursor(
    detail: &str,
) -> Result<Option<(String, ReclaimCursor)>, InvalidReclaimCursor> {
    let Some(saved) = detail.strip_prefix("reclaim:") else {
        return Ok(None);
    };
    let Some((class, serialized)) = saved.split_once(':') else {
        return Err(InvalidReclaimCursor);
    };
    if class.is_empty() {
        return Err(InvalidReclaimCursor);
    }
    let cursor = serde_json::from_str(serialized).map_err(|_| InvalidReclaimCursor)?;
    Ok(Some((class.to_owned(), cursor)))
}

#[derive(Debug)]
pub struct InvalidReclaimCursor;

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
        let mut saved = plan_page(None, stream(), 3).cursor;
        saved.mark_visited("a");
        saved.mark_visited("b");
        let resume = plan_page(Some(&saved), stream(), 3);
        assert_eq!(resume.cursor.page_start_seq, 0);
        assert_eq!(resume.cursor.page_end_seq, 3);
        assert_eq!(resume.candidates, vec![(2, "c")]);

        // With the whole page visited, the next plan opens page two rather than
        // revisiting any candidate from page one.
        saved.mark_visited("c");
        saved.close_page();
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
            if page.candidates.len() == 1 {
                cursor.close_page();
            }
            saved = Some(cursor);
        }
        assert_eq!(applied, ["a", "b", "c", "d", "e", "f", "g"]);
        assert_eq!(saved.map(|cursor| cursor.page_end_seq), Some(7));
    }

    #[test]
    fn resume_handles_sparse_sequences_and_deleted_visited_candidates() {
        let ranked = [(2, "a"), (11, "b"), (25, "c"), (80, "d")];
        let mut saved = plan_page(None, ranked, 3).cursor;
        assert_eq!((saved.page_start_seq, saved.page_end_seq), (2, 26));
        saved.mark_visited("a");
        saved.mark_visited("b");

        // The applied rows may have been deleted. The saved open range still
        // returns the lower-sequence outstanding candidate before sequence 80.
        let resumed = plan_page(Some(&saved), [(25, "c"), (80, "d")], 3);
        assert_eq!(resumed.candidates, [(25, "c")]);
        saved.mark_visited("c");
        saved.close_page();
        let next = plan_page(Some(&saved), [(80, "d")], 3);
        assert_eq!(next.candidates, [(80, "d")]);
    }

    #[test]
    fn cursor_continuation_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let cursor = ReclaimCursor {
            page_start_seq: 7,
            page_end_seq: 19,
            visited: vec!["attempt-a".into()],
        };
        let encoded = encode_cursor("scratch", &cursor)?;
        let decoded = decode_cursor(&encoded)
            .map_err(|_| std::io::Error::other("reclaim cursor did not parse"))?;
        assert_eq!(decoded, Some(("scratch".to_owned(), cursor)));
        Ok(())
    }
}
