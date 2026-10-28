//! Bounded daemon-lifetime audit retention and cursor paging.

use std::collections::VecDeque;

use splinterm_protocol::{
    AuditDecision, AuditOperation, AuditOutcome, AuditPage, AuditPeer, AuditRecord, AuditResource,
    AutomationScope, MAX_AUDIT_PAGE_RECORDS, Request,
};

const MAX_AUDIT_RECORDS: usize = 1024;
const MAX_REASON_BYTES: usize = 64;

#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "every request maps to its closed audit operation in one exhaustive table"
)]
pub const fn operation_for_request(request: &Request) -> AuditOperation {
    match request {
        Request::Ping => AuditOperation::Ping,
        Request::ListLairs => AuditOperation::ListLairs,
        Request::InspectTopology => AuditOperation::InspectTopology,
        Request::SubscribeTopology => AuditOperation::SubscribeTopology,
        Request::InspectSplint { .. } => AuditOperation::InspectSplint,
        Request::ResolveLaunchDirectory { .. } => AuditOperation::ResolveLaunchDirectory,
        Request::ReadGraphicalFocus => AuditOperation::ReadGraphicalFocus,
        Request::PublishGraphicalFocus { .. } => AuditOperation::PublishGraphicalFocus,
        Request::RequestAccess { .. } => AuditOperation::RequestAccess,
        // Cancelling is part of the workspace request's lifecycle; the audit
        // vocabulary stays closed.
        Request::RequestLairAccess { .. }
        | Request::CancelLairAccessRequest { .. }
        | Request::CancelAllLairAccessRequests => AuditOperation::RequestLairAccess,
        // The access inventory and its revokes reuse the closed public audit
        // vocabulary (`splinterm.audit.v2`): inspecting and revoking.
        Request::AuthorizationStatus { .. } | Request::ListAccess { .. } => {
            AuditOperation::AuthorizationStatus
        }
        Request::RevokeAccess { .. }
        | Request::RevokeAccessEntry { .. }
        | Request::RevokeAllAccess { .. } => AuditOperation::RevokeAccess,
        Request::PrepareMutation { mutation } => match mutation {
            splinterm_protocol::MutationPreflight::CreateLair => AuditOperation::CreateLair,
            splinterm_protocol::MutationPreflight::SplitSplint { .. } => {
                AuditOperation::SplitSplint
            }
            splinterm_protocol::MutationPreflight::NewDojo { .. } => AuditOperation::NewDojo,
            splinterm_protocol::MutationPreflight::RelaunchSplint { .. } => {
                AuditOperation::RelaunchSplint
            }
            splinterm_protocol::MutationPreflight::RestoreSplint { .. } => {
                AuditOperation::RestoreSplint
            }
            splinterm_protocol::MutationPreflight::RestoreDojo { .. } => {
                AuditOperation::RestoreDojo
            }
            splinterm_protocol::MutationPreflight::RestoreLair { .. } => {
                AuditOperation::RestoreLair
            }
            splinterm_protocol::MutationPreflight::CloseSplint { .. } => {
                AuditOperation::CloseSplint
            }
            splinterm_protocol::MutationPreflight::CloseDojo { .. } => AuditOperation::CloseDojo,
            splinterm_protocol::MutationPreflight::TerminateLair { .. } => {
                AuditOperation::TerminateLair
            }
            splinterm_protocol::MutationPreflight::KillSplint { .. } => AuditOperation::KillSplint,
            splinterm_protocol::MutationPreflight::SetSplitRatio { .. } => {
                AuditOperation::SetSplitRatio
            }
            splinterm_protocol::MutationPreflight::RenameLair { .. } => AuditOperation::RenameLair,
            splinterm_protocol::MutationPreflight::RenameDojo { .. } => AuditOperation::RenameDojo,
            splinterm_protocol::MutationPreflight::RenameSplint { .. } => {
                AuditOperation::RenameSplint
            }
            splinterm_protocol::MutationPreflight::SetDojoDefaultFocus { .. } => {
                AuditOperation::SetDojoDefaultFocus
            }
        },
        Request::CreateLair { .. }
        | Request::CreateTransientLair { .. }
        | Request::CreateLairAutomation { .. } => AuditOperation::CreateLair,
        Request::SplitSplint { .. } | Request::SplitSplintAutomation { .. } => {
            AuditOperation::SplitSplint
        }
        Request::RelaunchSplint { .. } | Request::RelaunchSplintAutomation { .. } => {
            AuditOperation::RelaunchSplint
        }
        Request::RestoreSplint { .. } => AuditOperation::RestoreSplint,
        Request::RestoreDojo { .. } => AuditOperation::RestoreDojo,
        Request::RestoreLair { .. } => AuditOperation::RestoreLair,
        Request::SetLairRetention { .. } => AuditOperation::SetLairRetention,
        Request::CloseSplint { .. } => AuditOperation::CloseSplint,
        Request::SetSplitRatio { .. } => AuditOperation::SetSplitRatio,
        Request::NewDojo { .. } | Request::NewDojoAutomation { .. } => AuditOperation::NewDojo,
        Request::MaterializePreset { .. } => AuditOperation::MaterializePreset,
        Request::CloseDojo { .. } | Request::CloseUnusedDojo { .. } => AuditOperation::CloseDojo,
        Request::TerminateLair { .. } => AuditOperation::TerminateLair,
        Request::RenameLair { .. } => AuditOperation::RenameLair,
        Request::RenameDojo { .. } => AuditOperation::RenameDojo,
        Request::SetDojoDefaultFocus { .. } => AuditOperation::SetDojoDefaultFocus,
        Request::RenameSplint { .. } => AuditOperation::RenameSplint,
        // Keep the frozen public audit vocabulary stable; content reads remain
        // an exact-incarnation terminal attachment operation in this phase.
        Request::Attach { .. } | Request::RequestImageContent { .. } => AuditOperation::Attach,
        Request::StartScrollbackPage { .. } | Request::ScrollbackPage { .. } => {
            AuditOperation::ScrollbackPage
        }
        Request::StartSearchScrollback { .. } | Request::SearchScrollback { .. } => {
            AuditOperation::SearchScrollback
        }
        Request::AcquireControl { .. } => AuditOperation::AcquireControl,
        Request::SubscribeControl { .. } => AuditOperation::SubscribeControl,
        Request::RequestControlTransfer { .. } => AuditOperation::RequestControlTransfer,
        Request::DecideControlTransfer { .. } => AuditOperation::DecideControlTransfer,
        Request::ForceControlTransfer { .. } => AuditOperation::ForceControlTransfer,
        Request::ReleaseControl { .. } => AuditOperation::ReleaseControl,
        Request::Input { .. } => AuditOperation::Input,
        Request::Resize { .. } => AuditOperation::Resize,
        Request::Detach { .. } => AuditOperation::Detach,
        Request::KillSplint { .. } => AuditOperation::KillSplint,
        Request::AuditInspect { .. } => AuditOperation::AuditInspect,
        Request::ReportAgentStatus { .. } => AuditOperation::AgentStatusReport,
        Request::PrepareDaemonHandoff { .. } => AuditOperation::PrepareDaemonHandoff,
        Request::ClaimHandoffControl { .. } => AuditOperation::ClaimHandoffControl,
        Request::AcknowledgeHandoffPrepare { .. } => AuditOperation::AcknowledgeHandoffPrepare,
        Request::SubscribeConsentOffers { .. } => AuditOperation::SubscribeConsentOffers,
        Request::AnswerConsentOffer { .. } => AuditOperation::AnswerConsentOffer,
    }
}

#[derive(Clone, Debug)]
pub struct AuditDraft {
    pub unix_seconds: u64,
    pub policy_generation: Option<u64>,
    pub policy_rule_id: Option<String>,
    pub peer: AuditPeer,
    pub operation: AuditOperation,
    pub resource: Option<AuditResource>,
    pub requested_scopes: Vec<AutomationScope>,
    pub decision: AuditDecision,
    pub reason: &'static str,
    pub outcome: Option<AuditOutcome>,
    pub argument_count: Option<usize>,
    pub executable_basename: Option<String>,
}

#[derive(Debug, Default)]
pub struct AuditStore {
    next_id: u64,
    records: VecDeque<AuditRecord>,
}

impl AuditStore {
    pub fn record(&mut self, draft: AuditDraft) -> u64 {
        self.next_id = self.next_id.saturating_add(1).max(1);
        let reason = sanitize_reason(draft.reason);
        let record = AuditRecord {
            schema: "splinterm.audit.v2".into(),
            retention: "daemon_lifetime".into(),
            audit_id: self.next_id,
            unix_seconds: draft.unix_seconds.max(1),
            policy_generation: draft.policy_generation,
            policy_rule_id: draft.policy_rule_id,
            peer: draft.peer,
            operation: draft.operation,
            resource: draft.resource,
            requested_scopes: draft.requested_scopes,
            decision: draft.decision,
            reason,
            outcome: draft.outcome,
            argument_count: draft.argument_count.map(|count| count.min(64)),
            executable_basename: draft
                .executable_basename
                .map(|basename| basename.chars().take(255).collect()),
        };
        self.records.push_back(record);
        while self.records.len() > MAX_AUDIT_RECORDS {
            self.records.pop_front();
        }
        self.next_id
    }

    pub fn page(&self, after_audit_id: Option<u64>, max_records: usize) -> AuditPage {
        let maximum = max_records.clamp(1, MAX_AUDIT_PAGE_RECORDS);
        let oldest = self.records.front().map(|record| record.audit_id);
        let newest = self.records.back().map(|record| record.audit_id);
        let requested_after = after_audit_id.unwrap_or(0);
        let retention_gap = oldest.is_some_and(|oldest| {
            requested_after > 0 && requested_after.saturating_add(1) < oldest
        });
        let effective_after = if retention_gap {
            oldest.unwrap_or(1).saturating_sub(1)
        } else {
            requested_after
        };
        let records = self
            .records
            .iter()
            .filter(|record| record.audit_id > effective_after)
            .take(maximum)
            .cloned()
            .collect::<Vec<_>>();
        let next_after_audit_id = records.last().map(|record| record.audit_id);
        AuditPage {
            records,
            retention_gap,
            oldest_available_audit_id: oldest,
            newest_available_audit_id: newest,
            next_after_audit_id,
        }
    }
}

fn sanitize_reason(reason: &str) -> String {
    let mut reason = reason
        .bytes()
        .take(MAX_REASON_BYTES)
        .map(|byte| {
            if byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect::<String>();
    if !reason.starts_with(|character: char| character.is_ascii_lowercase()) {
        reason.insert(0, 'x');
        reason.truncate(MAX_REASON_BYTES);
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_responder_requests_have_their_own_operations() {
        use splinterm_protocol::{
            CONSENT_OFFER_NONCE_BYTES, ConsentDecision, ConsentOfferNonce, ConsentResponderKind,
        };

        for kind in [ConsentResponderKind::Window, ConsentResponderKind::Terminal] {
            assert_eq!(
                operation_for_request(&Request::SubscribeConsentOffers { kind }),
                AuditOperation::SubscribeConsentOffers
            );
        }
        assert_eq!(
            operation_for_request(&Request::AnswerConsentOffer {
                daemon_generation: 1,
                offer_id: 1,
                nonce: ConsentOfferNonce::new([1; CONSENT_OFFER_NONCE_BYTES]),
                decision: ConsentDecision::Deny,
            }),
            AuditOperation::AnswerConsentOffer
        );
        assert_eq!(
            AuditOperation::SubscribeConsentOffers.as_str(),
            "subscribe_consent_offers"
        );
        assert_eq!(
            AuditOperation::AnswerConsentOffer.as_str(),
            "answer_consent_offer"
        );
    }

    fn peer() -> AuditPeer {
        AuditPeer {
            uid: 1000,
            executable_path: "/usr/bin/client".into(),
            executable_sha256: "a".repeat(64),
            device: Some(1),
            inode: Some(2),
        }
    }

    fn draft() -> AuditDraft {
        AuditDraft {
            unix_seconds: 1,
            policy_generation: Some(1),
            policy_rule_id: Some("reader".into()),
            peer: peer(),
            operation: AuditOperation::InspectTopology,
            resource: None,
            requested_scopes: vec![AutomationScope::TopologyMetadataRead],
            decision: AuditDecision::Allowed,
            reason: "policy_match",
            outcome: Some(AuditOutcome::Succeeded),
            argument_count: None,
            executable_basename: None,
        }
    }

    #[test]
    fn first_page_requests_reuse_reviewed_history_operations() {
        let splint_id = splinterm_core::SplintId::new();
        assert_eq!(
            operation_for_request(&Request::StartScrollbackPage {
                splint_id,
                incarnation: None,
                max_rows: 16,
            }),
            AuditOperation::ScrollbackPage
        );
        assert_eq!(
            operation_for_request(&Request::StartSearchScrollback {
                splint_id,
                incarnation: None,
                query: "needle".into(),
                case_sensitive: false,
                max_results: 16,
            }),
            AuditOperation::SearchScrollback
        );
        assert_eq!(
            operation_for_request(&Request::PrepareMutation {
                mutation: splinterm_protocol::MutationPreflight::SplitSplint { splint_id },
            }),
            AuditOperation::SplitSplint
        );
        assert_eq!(
            operation_for_request(&Request::PrepareMutation {
                mutation: splinterm_protocol::MutationPreflight::RenameSplint { splint_id },
            }),
            AuditOperation::RenameSplint
        );
    }

    #[test]
    fn cursor_pages_are_monotonic_and_report_retention_gaps() {
        let mut store = AuditStore::default();
        for _ in 0..=MAX_AUDIT_RECORDS {
            store.record(draft());
        }
        let first = store.page(Some(0), 2);
        assert!(!first.retention_gap);
        assert_eq!(first.records[0].audit_id, 2);
        assert_eq!(first.records[1].audit_id, 3);

        let gap = store.page(Some(1), 2);
        assert!(!gap.retention_gap);
        let gap = store.page(Some(0), 2);
        assert!(!gap.retention_gap);
        let stale = store.page(Some(1), 2);
        assert!(!stale.retention_gap);

        for _ in 0..3 {
            store.record(draft());
        }
        let stale = store.page(Some(1), 2);
        assert!(stale.retention_gap);
        assert_eq!(stale.records[0].audit_id, 5);
        assert_eq!(stale.oldest_available_audit_id, Some(5));
    }

    #[test]
    fn records_are_body_free_bounded_and_page_limited() {
        let mut store = AuditStore::default();
        let mut item = draft();
        item.reason = "Bad reason with spaces and terminal bytes";
        item.argument_count = Some(1000);
        item.executable_basename = Some("x".repeat(400));
        store.record(item);
        let page = store.page(None, usize::MAX);
        let record = &page.records[0];
        assert_eq!(record.reason, "x_ad_reason_with_spaces_and_terminal_bytes");
        assert_eq!(record.argument_count, Some(64));
        assert_eq!(record.executable_basename.as_ref().unwrap().len(), 255);
        assert!(page.records.len() <= MAX_AUDIT_PAGE_RECORDS);
    }
}
