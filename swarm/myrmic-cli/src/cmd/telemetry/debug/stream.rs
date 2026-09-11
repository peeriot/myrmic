use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use db_commons::models::{Cursor, Id};
use uuid::Builder;

use super::data::DebugItem;

/// What a log notification asks of the log table.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LogRead {
    /// List the table from this cursor; `None` lists it from the beginning.
    From(Option<Cursor>),
    /// Nothing to anchor a read to, so the notification is dropped.
    Skip,
}

/// The debug stream's ordering state: where the next log read starts, and the command and
/// event items waiting for a log row to place them against. Carries no I/O.
pub(crate) struct DebugStream {
    queue: BTreeSet<DebugItem>,
    log_cursor: Option<Cursor>,
}

impl DebugStream {
    pub(crate) fn new(log_cursor: Option<Cursor>) -> Self {
        Self {
            queue: BTreeSet::new(),
            log_cursor,
        }
    }

    pub(crate) fn push(&mut self, item: DebugItem) {
        self.queue.insert(item);
    }

    /// Where the next log read starts. Without a cursor the oldest queued item's timestamp
    /// stands in for one, and with nothing queued there is nothing to read against.
    pub(crate) fn next_log_read(&self) -> anyhow::Result<LogRead> {
        if let Some(cursor) = &self.log_cursor {
            return Ok(LogRead::From(Some(cursor.clone())));
        }

        let Some(first) = self.queue.first() else {
            return Ok(LogRead::Skip);
        };

        let millis: u64 = first
            .timestamp()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        let id = Builder::from_unix_timestamp_millis(millis, &[0u8; 10]).into_uuid();

        Ok(LogRead::From(Some(Cursor::After(id.as_bytes().to_vec()))))
    }

    /// Continues the next read past this log row.
    pub(crate) fn advance(&mut self, id: &Id) {
        self.log_cursor = Some(Cursor::After(id.clone()));
    }

    /// Takes the queued items a log row emitted at `emitted_at` releases, oldest first.
    pub(crate) fn flush_before(
        &mut self,
        emitted_at: SystemTime,
        sri_filter: Option<&str>,
    ) -> Vec<DebugItem> {
        self.take(Some(emitted_at), sri_filter)
    }

    /// Takes every queued item, oldest first, without applying the cell filter.
    pub(crate) fn drain_all(&mut self) -> Vec<DebugItem> {
        self.take(None, None)
    }

    fn take(&mut self, boundary: Option<SystemTime>, sri_filter: Option<&str>) -> Vec<DebugItem> {
        let mut taken = Vec::new();

        while self
            .queue
            .first()
            .is_some_and(|item| boundary.is_none_or(|boundary| *item.timestamp() <= boundary))
        {
            let Some(item) = self.queue.pop_first() else {
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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use cell_protocol::Sri;
    use uuid::{Builder, Uuid};

    use super::{Cursor, DebugItem, DebugStream, Id, LogRead};

    #[test]
    fn a_log_row_moves_the_next_read_past_it() {
        let mut stream = DebugStream::new(None);
        let row = row_id(1_000);

        stream.advance(&row);

        assert_eq!(
            stream.next_log_read().unwrap(),
            LogRead::From(Some(Cursor::After(row)))
        );
    }

    #[test]
    fn a_log_row_releases_the_items_queued_before_it() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));
        stream.push(DebugItem::command_at(2_000, sri(1)));
        stream.push(DebugItem::command_at(3_000, sri(1)));

        let released = stream.flush_before(at(2_000), None);

        assert_eq!(
            released,
            vec![
                DebugItem::command_at(1_000, sri(1)),
                DebugItem::command_at(2_000, sri(1)),
            ]
        );
        assert_eq!(
            stream.drain_all(),
            vec![DebugItem::command_at(3_000, sri(1))]
        );
    }

    #[test]
    fn the_anchor_the_stream_started_from_is_the_first_cursor() {
        let anchor = row_id(1_000);
        let stream = DebugStream::new(Some(Cursor::After(anchor.clone())));

        assert_eq!(
            stream.next_log_read().unwrap(),
            LogRead::From(Some(Cursor::After(anchor)))
        );
    }

    #[test]
    fn the_cursor_only_moves_forward() {
        let mut stream = DebugStream::new(Some(Cursor::After(row_id(1_000))));
        stream.advance(&row_id(2_000));
        stream.advance(&row_id(3_000));

        assert_eq!(
            stream.next_log_read().unwrap(),
            LogRead::From(Some(Cursor::After(row_id(3_000))))
        );
    }

    #[test]
    fn a_released_item_for_another_cell_is_dropped_not_printed() {
        let mut stream = DebugStream::new(None);
        stream.push(DebugItem::command_at(1_000, sri(1)));
        stream.push(DebugItem::command_at(1_500, sri(2)));

        let released = stream.flush_before(at(2_000), Some(&sri(1).to_string()));

        assert_eq!(released, vec![DebugItem::command_at(1_000, sri(1))]);
        assert_eq!(stream.drain_all(), Vec::new());
    }

    fn sri(n: u128) -> Sri {
        Sri::from_uuid(Uuid::from_u128(n))
    }

    fn at(millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(millis)
    }

    fn row_id(millis: u64) -> Id {
        Builder::from_unix_timestamp_millis(millis, &[0u8; 10])
            .into_uuid()
            .as_bytes()
            .to_vec()
    }
}
