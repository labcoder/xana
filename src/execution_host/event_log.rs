use super::{ExecutionHostSnapshot, HostEvent, HostObservation};
use std::collections::VecDeque;

const MAX_RETAINED_EVENTS: usize = 512;
const MAX_RETAINED_BYTES: usize = 4 * 1024 * 1024;

pub(super) struct EventLog {
    sequence: u64,
    retained_bytes: usize,
    events: VecDeque<(usize, HostObservation)>,
}

pub(super) enum Changes {
    Events(Vec<HostObservation>),
    SnapshotRequired(ExecutionHostSnapshot),
}

impl EventLog {
    pub(super) fn new() -> Self {
        Self {
            sequence: 0,
            retained_bytes: 0,
            events: VecDeque::new(),
        }
    }

    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn push(&mut self, event: HostEvent) -> HostObservation {
        self.sequence = self.sequence.saturating_add(1);
        let observation = HostObservation {
            sequence: self.sequence,
            event,
        };
        let encoded_bytes = serde_json::to_vec(&observation).map_or(0, |bytes| bytes.len());
        if encoded_bytes <= MAX_RETAINED_BYTES {
            self.events.push_back((encoded_bytes, observation.clone()));
            self.retained_bytes = self.retained_bytes.saturating_add(encoded_bytes);
            while self.events.len() > MAX_RETAINED_EVENTS
                || self.retained_bytes > MAX_RETAINED_BYTES
            {
                if let Some((removed_bytes, _)) = self.events.pop_front() {
                    self.retained_bytes = self.retained_bytes.saturating_sub(removed_bytes);
                } else {
                    break;
                }
            }
        }
        observation
    }

    pub(super) fn changes_after(
        &self,
        cursor: u64,
        snapshot: impl FnOnce() -> ExecutionHostSnapshot,
    ) -> Changes {
        if cursor == self.sequence {
            return Changes::Events(Vec::new());
        }
        let first = self.events.front().map(|(_, event)| event.sequence);
        if cursor > self.sequence
            || first.is_none()
            || first.is_some_and(|first| cursor.saturating_add(1) < first)
        {
            return Changes::SnapshotRequired(snapshot());
        }
        Changes::Events(
            self.events
                .iter()
                .filter(|(_, event)| event.sequence > cursor)
                .map(|(_, event)| event.clone())
                .collect(),
        )
    }
}
