use katwork::{
    agent::{RunFacts, StopReason, ToolCallRecord, ToolCallState},
    events::KatworkEvent,
    ids::*,
    policy::{self, NextStep, PolicyInputs, PolicyStatus, Thresholds, Verdict},
    types::{JobStatus, OutwardKind, Question, ReasonCode, SideEffect},
};

fn cases() -> Vec<(StopReason, PolicyStatus, ReasonCode, NextStep)> {
    use NextStep::*;
    use PolicyStatus::{NeedsInput, Stopped};
    use ReasonCode as R;
    use StopReason as S;
    vec![
        (
            S::ProviderUnavailable,
            Stopped,
            R::ProviderUnavailable,
            Retry,
        ),
        (
            S::ProviderRateLimited,
            Stopped,
            R::ProviderRateLimited,
            Retry,
        ),
        (S::ProviderQuota, Stopped, R::ProviderQuota, Retry),
        (
            S::ProviderAuthFailed,
            Stopped,
            R::ProviderAuthFailed,
            AskUser,
        ),
        (S::ModelProtocol, Stopped, R::ModelProtocolError, Retry),
        (S::TimeLimit, Stopped, R::TimeLimit, Retry),
        (S::QuotaExceeded, Stopped, R::QuotaExceeded, Retry),
        (S::TooManyRequests, Stopped, R::TooManyRequests, Retry),
        (S::WorkspacePaused, Stopped, R::WorkspacePaused, Retry),
        (S::KatPaused, Stopped, R::KatPaused, AskUser),
        (S::ResidencyDenied, Stopped, R::ResidencyDenied, AskUser),
        (S::NoEligibleKat, Stopped, R::NoEligibleKat, AskUser),
        (S::StepLimit, Stopped, R::StepLimit, SaveDraftWithReport),
        (S::TokenBudget, Stopped, R::TokenBudget, SaveDraftWithReport),
        (
            S::OutputTruncated,
            Stopped,
            R::OutputTruncated,
            SaveDraftWithReport,
        ),
        (S::LoopDetected, Stopped, R::LoopDetected, HandToLead),
        (S::PolicyChanged, Stopped, R::PolicyChanged, RestartJob),
        (S::NeedsApproval, NeedsInput, R::ApprovalPending, AskUser),
        (
            S::Interrupted,
            Stopped,
            R::InterruptedReviewRequired,
            ReviewBeforeRetry,
        ),
        (
            S::OutcomeUnknown,
            Stopped,
            R::OutcomeUnknown,
            ReviewBeforeRetry,
        ),
        (S::Cancelled, Stopped, R::Cancelled, Retry),
    ]
}
fn verdict(status: PolicyStatus, reason: ReasonCode, next: NextStep) -> Verdict {
    Verdict {
        status,
        reason: Some(reason),
        next: Some(next),
    }
}
fn call(effect: Option<SideEffect>, state: ToolCallState) -> ToolCallRecord {
    ToolCallRecord {
        id: ToolCallRecordId::new(),
        step: 1,
        tool_call_id: "call".into(),
        name: "tool".into(),
        side_effect: effect,
        state,
    }
}
fn answered() -> RunFacts {
    let mut f = RunFacts::not_run(StopReason::ModelProtocol);
    f.stop = None;
    f.final_text = Some("คำตอบ".into());
    f
}
#[test]
fn every_stop_reason_maps_to_its_reason_code() {
    for (stop, status, reason, next) in cases() {
        assert_eq!(
            policy::decide_job(&RunFacts::not_run(stop), &Thresholds::SP1),
            verdict(status, reason, next),
            "{stop:?}"
        );
    }
}
#[test]
fn policy_same_facts_same_verdict() {
    let f = answered();
    assert_eq!(
        policy::decide_job(&f, &Thresholds::SP1),
        policy::decide_job(&f.clone(), &Thresholds::SP1)
    );
}
#[test]
fn answered_only_without_artifacts_else_ready() {
    let mut f = answered();
    for (n, status) in [
        (0, PolicyStatus::Answered),
        (1, PolicyStatus::Ready),
        (u32::MAX, PolicyStatus::Ready),
    ] {
        f.artifacts_created = n;
        assert_eq!(
            policy::decide_job(&f, &Thresholds::SP1),
            Verdict {
                status,
                reason: None,
                next: None
            }
        );
    }
}
#[test]
fn executor_unavailable_is_stopped_provider_unavailable() {
    let mut f = answered();
    f.executor_unavailable = true;
    f.stop = Some(StopReason::NeedsApproval);
    assert_eq!(
        policy::decide_job(&f, &Thresholds::SP1),
        verdict(
            PolicyStatus::Stopped,
            ReasonCode::ProviderUnavailable,
            NextStep::Retry
        )
    );
}
#[test]
fn needs_approval_is_needs_input_approval_pending() {
    assert_eq!(
        policy::decide_job(
            &RunFacts::not_run(StopReason::NeedsApproval),
            &Thresholds::SP1
        ),
        verdict(
            PolicyStatus::NeedsInput,
            ReasonCode::ApprovalPending,
            NextStep::AskUser
        )
    );
}
#[test]
fn policy_status_converts_to_the_five_policy_owned_job_statuses() {
    for (status, job) in [
        (PolicyStatus::Accepted, JobStatus::Accepted),
        (PolicyStatus::Ready, JobStatus::Ready),
        (PolicyStatus::Answered, JobStatus::Answered),
        (PolicyStatus::Stopped, JobStatus::Stopped),
        (PolicyStatus::NeedsInput, JobStatus::NeedsInput),
    ] {
        assert_eq!(JobStatus::from(status), job);
    }
}
#[test]
fn finish_by_policy_rejects_cancelled_and_error() {
    for status in ["cancelled", "error"] {
        assert!(
            serde_json::from_value::<Verdict>(
                serde_json::json!({"status":status,"reason":null,"next":null})
            )
            .is_err()
        );
    }
}
#[test]
fn provider_failure_after_write_requires_review() {
    let mut f = RunFacts::not_run(StopReason::ProviderUnavailable);
    for (effect, state, next) in [
        (
            SideEffect::WorkspaceWrite,
            ToolCallState::Succeeded,
            NextStep::ReviewBeforeRetry,
        ),
        (SideEffect::Read, ToolCallState::Succeeded, NextStep::Retry),
        (
            SideEffect::WorkspaceWrite,
            ToolCallState::Refused,
            NextStep::Retry,
        ),
    ] {
        f.tool_calls = vec![call(Some(effect), state)];
        assert_eq!(
            policy::decide_job(&f, &Thresholds::SP1),
            verdict(PolicyStatus::Stopped, ReasonCode::ProviderUnavailable, next)
        );
    }
}
#[test]
fn decision_event_replays_same_verdict() {
    for (stop, _, _, _) in cases() {
        let f = RunFacts::not_run(stop);
        let v = policy::decide_job(&f, &Thresholds::SP1);
        let KatworkEvent::Decision(event) =
            policy::decision_event(JobId::new(), &f, &v, &Thresholds::SP1)
        else {
            panic!("decision expected")
        };
        let inputs: PolicyInputs = serde_json::from_value(event.inputs).unwrap();
        let thresholds: Thresholds = serde_json::from_value(event.thresholds).unwrap();
        assert_eq!(thresholds.version, event.policy_version);
        assert_eq!(
            serde_json::to_value(policy::decide(&inputs, &thresholds)).unwrap(),
            event.verdict
        );
    }
}
#[test]
fn policy_table_covers_fact_combinations_and_every_call_state_and_effect() {
    let mut rows = cases()
        .into_iter()
        .map(|(s, status, reason, next)| (Some(s), verdict(status, reason, next)))
        .collect::<Vec<_>>();
    rows.push((
        None,
        verdict(
            PolicyStatus::Stopped,
            ReasonCode::ModelProtocolError,
            NextStep::Retry,
        ),
    ));
    let effects = [
        None,
        Some(SideEffect::Read),
        Some(SideEffect::Navigate),
        Some(SideEffect::WorkspaceRead),
        Some(SideEffect::Draft),
        Some(SideEffect::WorkspaceWrite),
        Some(SideEffect::Exec),
        Some(SideEffect::CredentialUse),
        Some(SideEffect::Destructive),
        Some(SideEffect::Outward(OutwardKind::Send)),
        Some(SideEffect::Outward(OutwardKind::Publish)),
        Some(SideEffect::Outward(OutwardKind::Pay)),
        Some(SideEffect::Outward(OutwardKind::Share)),
        Some(SideEffect::Outward(OutwardKind::ExternalApi)),
    ];
    for (stop, base) in rows {
        for bits in 0..128 {
            let mut f = answered();
            f.stop = stop;
            f.executor_unavailable = bits & 1 != 0;
            f.final_text = (bits & 2 != 0).then(|| "answer".into());
            f.pending_question = (bits & 4 != 0).then(|| Question {
                question: "Which?".into(),
                options: vec![],
            });
            f.artifacts_created = u32::from(bits & 8 != 0);
            f.synthetic = bits & 16 != 0;
            f.run_closed = bits & 32 != 0;
            f.run_id = (bits & 64 != 0).then(RunId::new);
            let expected = if f.executor_unavailable {
                verdict(
                    PolicyStatus::Stopped,
                    ReasonCode::ProviderUnavailable,
                    NextStep::Retry,
                )
            } else if stop.is_some() {
                base.clone()
            } else if f.pending_question.is_some() {
                verdict(
                    PolicyStatus::NeedsInput,
                    ReasonCode::QuestionPending,
                    NextStep::AskUser,
                )
            } else if f.final_text.is_some() {
                Verdict {
                    status: if f.artifacts_created == 0 {
                        PolicyStatus::Answered
                    } else {
                        PolicyStatus::Ready
                    },
                    reason: None,
                    next: None,
                }
            } else {
                base.clone()
            };
            assert_eq!(policy::decide_job(&f, &Thresholds::SP1), expected);
            for effect in effects {
                for state in [
                    ToolCallState::Started,
                    ToolCallState::Succeeded,
                    ToolCallState::Failed,
                    ToolCallState::TimedOut,
                    ToolCallState::OutcomeUnknown,
                    ToolCallState::Refused,
                ] {
                    let review =
                        effect != Some(SideEffect::Read) && state != ToolCallState::Refused;
                    f.tool_calls = vec![call(effect, state)];
                    let inputs = PolicyInputs::from_facts(&f);
                    assert_eq!(inputs.effects_dispatched, review);
                    assert_eq!(inputs.tool_calls, 1);
                    let mut wanted = expected.clone();
                    if review && wanted.next == Some(NextStep::Retry) {
                        wanted.next = Some(NextStep::ReviewBeforeRetry);
                    }
                    assert_eq!(
                        policy::decide_job(&f, &Thresholds::SP1),
                        wanted,
                        "{stop:?} bits={bits}, {effect:?}, {state:?}"
                    );
                }
            }
        }
    }
}
