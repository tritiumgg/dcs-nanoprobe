//! The rule of one retention pass, shared by the journal, the event history
//! and their in-memory forms (spec/persistence.md, "Journal retention" and
//! "Event history").

/// One row of the oldest rows of a table: its `seq`, its time in
/// milliseconds and the bytes of its body.
pub type Row = (u64, u64, u64);

/// What one pass deletes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cut {
    /// The last `seq` to delete: every row up to it goes. `None` when
    /// nothing is deleted.
    pub last: Option<u64>,
    /// How many rows that is.
    pub rows: u64,
    /// The bytes those rows hold.
    pub bytes: u64,
    /// More rows may remain to delete: the batch was full and every row in
    /// it was deleted.
    pub more: bool,
}

/// Decide what one pass deletes from `batch`, the oldest rows in `seq`
/// order, at most `max_rows` of them. `newest_seq` is the newest row of the
/// whole table; it is never deleted. `cutoff_ms` is the time rule: a row
/// older than it is deleted; `None` turns the time rule off. `excess` is the
/// size rule: rows are deleted while the bytes deleted so far are below it;
/// 0 turns the size rule off.
///
/// The walk goes in order and stops at the first row neither rule takes. The
/// time rule takes a prefix: it ends at the first row not older than the
/// cutoff, even if a later row is older. The size rule goes on past it.
pub fn cut(
    batch: &[Row],
    max_rows: usize,
    newest_seq: u64,
    cutoff_ms: Option<u64>,
    excess: u64,
) -> Cut {
    let mut out = Cut::default();
    let mut by_time = cutoff_ms.is_some();
    for &(seq, time_ms, bytes) in batch {
        if seq >= newest_seq {
            break;
        }
        if by_time && !cutoff_ms.is_some_and(|c| time_ms < c) {
            by_time = false;
        }
        if !by_time && out.bytes >= excess {
            break;
        }
        out.last = Some(seq);
        out.rows += 1;
        out.bytes = out.bytes.saturating_add(bytes);
    }
    out.more = max_rows > 0 && batch.len() == max_rows && out.rows == batch.len() as u64;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows 1..=n, 100 ms apart from 100, 10 bytes each.
    fn rows(n: u64) -> Vec<Row> {
        (1..=n).map(|s| (s, s * 100, 10)).collect()
    }

    #[test]
    fn nothing_to_do_deletes_nothing() {
        let c = cut(&rows(5), 100, 5, None, 0);
        assert_eq!(c, Cut::default());
    }

    #[test]
    fn size_only_deletes_until_the_excess_is_covered() {
        let c = cut(&rows(5), 100, 5, None, 25);
        // 10, 20, 30: the third row takes the total past 25.
        assert_eq!((c.last, c.rows, c.bytes, c.more), (Some(3), 3, 30, false));
        let c = cut(&rows(5), 100, 5, None, 20);
        assert_eq!((c.last, c.bytes), (Some(2), 20));
    }

    #[test]
    fn time_only_deletes_the_rows_older_than_the_cutoff() {
        let c = cut(&rows(5), 100, 5, Some(300), 0);
        assert_eq!((c.last, c.rows, c.bytes), (Some(2), 2, 20));
        let c = cut(&rows(5), 100, 5, Some(100), 0);
        assert_eq!(c, Cut::default());
    }

    #[test]
    fn both_rules_delete_what_either_takes() {
        // Time takes 2 rows; the size excess takes 4.
        let c = cut(&rows(8), 100, 8, Some(300), 35);
        assert_eq!((c.last, c.bytes), (Some(4), 40));
        // Time takes 4 rows; the size excess takes 1.
        let c = cut(&rows(8), 100, 8, Some(500), 5);
        assert_eq!((c.last, c.bytes), (Some(4), 40));
    }

    #[test]
    fn the_newest_row_is_never_deleted() {
        let c = cut(&rows(3), 100, 3, Some(u64::MAX), u64::MAX);
        assert_eq!((c.last, c.rows, c.bytes), (Some(2), 2, 20));
        let c = cut(&rows(1), 100, 1, Some(u64::MAX), u64::MAX);
        assert_eq!(c, Cut::default());
    }

    #[test]
    fn an_out_of_order_time_ends_the_time_rule_but_not_the_size_rule() {
        // Row 3 is newer than the cutoff; row 4 is old again.
        let batch = [
            (1, 100, 10),
            (2, 100, 10),
            (3, 900, 10),
            (4, 100, 10),
            (5, 100, 10),
        ];
        let c = cut(&batch, 100, 5, Some(500), 0);
        assert_eq!((c.last, c.rows), (Some(2), 2));
        // With an excess the size rule goes on past row 3.
        let c = cut(&batch, 100, 5, Some(500), 35);
        assert_eq!((c.last, c.rows, c.bytes), (Some(4), 4, 40));
    }

    #[test]
    fn an_empty_batch_deletes_nothing() {
        let c = cut(&[], 10, 5, Some(1_000), 100);
        assert_eq!(c, Cut::default());
    }

    #[test]
    fn a_batch_smaller_than_the_excess_deletes_it_all_and_may_have_more() {
        let c = cut(&rows(4), 4, 9, None, 1_000);
        assert_eq!((c.last, c.rows, c.bytes, c.more), (Some(4), 4, 40, true));
        // A batch that is not full has no more.
        let c = cut(&rows(4), 5, 9, None, 1_000);
        assert!(!c.more);
    }

    #[test]
    fn more_needs_a_full_batch_that_was_deleted_whole() {
        // Full, but the walk stopped early.
        let c = cut(&rows(4), 4, 9, Some(250), 0);
        assert_eq!((c.rows, c.more), (2, false));
        // Full, and the newest row is in it.
        let c = cut(&rows(4), 4, 4, None, 1_000);
        assert_eq!((c.rows, c.more), (3, false));
    }
}
