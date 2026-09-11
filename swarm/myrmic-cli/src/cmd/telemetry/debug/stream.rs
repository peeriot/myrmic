use std::collections::BTreeMap;
use std::time::SystemTime;

use db_commons::models::{Cursor, Id};

use super::data::{DebugItem, insertion_time};

/// The debug stream's ordering state: where the next log read starts, and the command and
/// event items waiting for a log row to place them against. Carries no I/O.
pub(crate) struct DebugStream {
    /// Keyed by insertion time plus arrival order, because two rows can share a millisecond
    /// and a set keyed on the timestamp alone would keep only one of them.
    queue: BTreeMap<(SystemTime, u64), DebugItem>,
    arrived: u64,
    log_cursor: Option<Cursor>,
}

impl DebugStream {
    pub(crate) fn new(log_cursor: Option<Cursor>) -> Self {
        Self {
            queue: BTreeMap::new(),
            arrived: 0,
            log_cursor,
        }
    }

    pub(crate) fn push(&mut self, item: DebugItem) {
        let key = (*item.timestamp(), self.arrived);
        self.arrived += 1;

        self.queue.insert(key, item);
    }

    /// Where the next log read starts. `None` reads the table from the beginning.
    pub(crate) fn log_cursor(&self) -> Option<Cursor> {
        self.log_cursor.clone()
    }

    /// Continues the next read past this log row.
    pub(crate) fn advance(&mut self, id: &Id) {
        self.log_cursor = Some(Cursor::After(id.clone()));
    }

    /// Takes the queued items a log row releases, oldest first. The row's own insertion time is
    /// the boundary, so both sides of the comparison come from the database's clock.
    pub(crate) fn flush_before(&mut self, row_id: &Id, sri_filter: Option<&str>) -> Vec<DebugItem> {
        let Some(boundary) = insertion_time(row_id) else {
            return Vec::new();
        };

        self.take(Some(boundary), sri_filter)
    }

    /// Takes every queued item the cell filter admits, oldest first.
    pub(crate) fn drain_all(&mut self, sri_filter: Option<&str>) -> Vec<DebugItem> {
        self.take(None, sri_filter)
    }

    fn take(&mut self, boundary: Option<SystemTime>, sri_filter: Option<&str>) -> Vec<DebugItem> {
        let mut taken = Vec::new();

        while self
            .queue
            .first_key_value()
            .is_some_and(|((at, _), _)| boundary.is_none_or(|boundary| *at <= boundary))
        {
            let Some((_, item)) = self.queue.pop_first() else {
                break;
            };

            if item.filter_sri(sri_filter) {
                taken.push(item);
            }
        }

        taken
    }
}

#[cfg(test)]
mod tests {
    use cell_protocol::Sri;
    use uuid::{Builder, Uuid};

    use super::{Cursor, DebugItem, DebugStream, Id};

    #[test]
    fn a_notification_is_answered_even_with_nothing_queued() {
        let stream = DebugStream::new(None);

        assert_eq!(stream.log_cursor(), None);
    }

    #[test]
    fn a_queued_item_never_becomes_the_log_cursor() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));

        assert_eq!(stream.log_cursor(), None);
    }

    #[test]
    fn a_log_read_that_returned_nothing_leaves_the_next_read_alone() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));
        // the read that followed came back empty, so the queue was printed and emptied
        stream.drain_all(None);

        assert_eq!(stream.log_cursor(), None);
    }

    #[test]
    fn a_log_row_moves_the_next_read_past_it() {
        let mut stream = DebugStream::new(None);
        let row = row_id(1_000);

        stream.advance(&row);

        assert_eq!(stream.log_cursor(), Some(Cursor::After(row)));
    }

    #[test]
    fn a_log_row_releases_the_items_queued_before_it() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));
        stream.push(DebugItem::command_at(2_000, sri(1)));
        stream.push(DebugItem::command_at(3_000, sri(1)));

        let released = stream.flush_before(&row_id(2_000), None);

        assert_eq!(
            released,
            vec![
                DebugItem::command_at(1_000, sri(1)),
                DebugItem::command_at(2_000, sri(1)),
            ]
        );
        assert_eq!(
            stream.drain_all(None),
            vec![DebugItem::command_at(3_000, sri(1))]
        );
    }

    #[test]
    fn the_anchor_the_stream_started_from_is_the_first_cursor() {
        let anchor = row_id(1_000);
        let stream = DebugStream::new(Some(Cursor::After(anchor.clone())));

        assert_eq!(stream.log_cursor(), Some(Cursor::After(anchor)));
    }

    #[test]
    fn the_cursor_only_moves_forward() {
        let mut stream = DebugStream::new(Some(Cursor::After(row_id(1_000))));
        stream.advance(&row_id(2_000));
        stream.advance(&row_id(3_000));

        assert_eq!(stream.log_cursor(), Some(Cursor::After(row_id(3_000))));
    }

    #[test]
    fn a_released_item_for_another_cell_is_dropped_not_printed() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));
        stream.push(DebugItem::command_at(1_500, sri(2)));

        let released = stream.flush_before(&row_id(2_000), Some(&sri(1).to_string()));

        assert_eq!(released, vec![DebugItem::command_at(1_000, sri(1))]);
        assert_eq!(stream.drain_all(None), Vec::new());
    }

    #[test]
    fn a_drain_with_no_new_log_rows_still_honours_the_cell_filter() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::event_at(1_000));
        stream.push(DebugItem::command_at(2_000, sri(2)));
        stream.push(DebugItem::command_at(3_000, sri(1)));

        let drained = stream.drain_all(Some(&sri(1).to_string()));

        assert_eq!(drained, vec![DebugItem::command_at(3_000, sri(1))]);
    }

    #[test]
    fn two_items_in_the_same_millisecond_both_survive() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));
        stream.push(DebugItem::command_at(1_000, sri(2)));

        assert_eq!(
            stream.drain_all(None),
            vec![
                DebugItem::command_at(1_000, sri(1)),
                DebugItem::command_at(1_000, sri(2)),
            ]
        );
    }

    #[test]
    fn a_late_written_log_row_still_releases_the_items_before_it() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(2_000, sri(1)));

        // the batch processor held this record for a second before it was written
        let released = stream.flush_before(&row_id(2_500), None);

        assert_eq!(released, vec![DebugItem::command_at(2_000, sri(1))]);
    }

    fn sri(n: u128) -> Sri {
        Sri::from_uuid(Uuid::from_u128(n))
    }

    fn row_id(millis: u64) -> Id {
        Builder::from_unix_timestamp_millis(millis, &[0u8; 10])
            .into_uuid()
            .as_bytes()
            .to_vec()
    }
}
