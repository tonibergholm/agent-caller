//! Deferred request ends that wait for an in-flight answer claim to release.
use std::collections::HashMap;

use acd_protocol::{RequestStatus, ResolvedBy};

#[derive(Default)]
pub(super) struct DeferredEnds(HashMap<String, ResolvedBy>);

impl DeferredEnds {
    pub(super) fn enqueue(&mut self, request_id: String, by: ResolvedBy) {
        self.0.insert(request_id, by);
    }

    #[cfg(test)]
    fn enqueue_if_answering(&mut self, request_id: String, by: ResolvedBy, status: RequestStatus) {
        if status == RequestStatus::Answering {
            self.enqueue(request_id, by);
        }
    }

    #[cfg(test)]
    fn legacy_tick_retry(
        &mut self,
        request_id: &str,
        by: ResolvedBy,
        status_read_before_conditional_end: RequestStatus,
        status_after_conditional_end: RequestStatus,
    ) {
        if status_read_before_conditional_end == RequestStatus::Open
            && status_after_conditional_end == RequestStatus::Open
        {
            self.enqueue(request_id.to_owned(), by);
        }
    }

    /// Returns requests that are open now; an answer claim may have changed
    /// the status since the caller last read it.
    pub(super) fn ready_to_end(
        &mut self,
        mut status: impl FnMut(&str) -> Option<RequestStatus>,
    ) -> Vec<(String, ResolvedBy)> {
        let mut ready = Vec::new();
        self.0.retain(|request_id, by| match status(request_id) {
            None => false,
            Some(RequestStatus::Open) => {
                ready.push((request_id.clone(), *by));
                false
            }
            Some(RequestStatus::Answering | RequestStatus::Resolved) => true,
        });
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_between_end_and_status_read_is_retried() {
        // The first conditional end saw Answering and did nothing; the
        // request was released before the deferred queue is next inspected.
        let mut ends = DeferredEnds::default();
        ends.enqueue("released".into(), ResolvedBy::Terminal);

        assert_eq!(
            ends.ready_to_end(|id| match id {
                "released" => Some(RequestStatus::Open),
                _ => None,
            }),
            vec![("released".into(), ResolvedBy::Terminal)]
        );
    }

    #[test]
    fn release_is_lost_with_pre_fix_conditional_enqueue() {
        let mut ends = DeferredEnds::default();
        // Old code only enqueued after reading Answering. A release between
        // the end observation and that read therefore left nothing to retry.
        ends.enqueue_if_answering("released".into(), ResolvedBy::Terminal, RequestStatus::Open);
        assert!(ends.ready_to_end(|_| Some(RequestStatus::Open)).is_empty());
    }

    #[test]
    fn claim_between_tick_read_and_conditional_end_is_retained() {
        let mut ends = DeferredEnds::default();
        ends.enqueue("claimed".into(), ResolvedBy::Lost);

        // Tick read Open, then a claim won before RequestEndedIfOpen.
        assert!(
            ends.ready_to_end(|_| Some(RequestStatus::Answering))
                .is_empty()
        );
        assert_eq!(
            ends.ready_to_end(|_| Some(RequestStatus::Open)),
            vec![("claimed".into(), ResolvedBy::Lost)]
        );
    }

    #[test]
    fn claim_is_lost_with_pre_fix_tick_retry() {
        let mut ends = DeferredEnds::default();
        // Legacy on_tick read Open, then a claim won before its conditional
        // end; legacy code removed the queue entry regardless of outcome.
        ends.legacy_tick_retry(
            "claimed",
            ResolvedBy::Lost,
            RequestStatus::Open,
            RequestStatus::Answering,
        );
        assert!(ends.ready_to_end(|_| Some(RequestStatus::Open)).is_empty());
    }

    #[test]
    fn retains_existing_drops_gone_and_never_ends_answering() {
        let mut ends = DeferredEnds::default();
        ends.enqueue("answering".into(), ResolvedBy::Terminal);
        ends.enqueue("gone".into(), ResolvedBy::Lost);

        assert!(
            ends.ready_to_end(|id| match id {
                "answering" => Some(RequestStatus::Answering),
                _ => None,
            })
            .is_empty()
        );
        assert!(
            ends.ready_to_end(|id| match id {
                "answering" => Some(RequestStatus::Answering),
                _ => None,
            })
            .is_empty()
        );
        assert_eq!(
            ends.ready_to_end(|_| Some(RequestStatus::Open)),
            vec![("answering".into(), ResolvedBy::Terminal)]
        );
        assert!(ends.ready_to_end(|_| None).is_empty());
    }
}
