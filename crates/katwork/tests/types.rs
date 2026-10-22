use katwork::{ids::*, types::*};
use serde_json::{from_value, json, to_value};

#[test]
fn typed_ids_are_uuid_v7_and_round_trip() {
    macro_rules! check {
        ($($id:ident),+ $(,)?) => {$({
            let id = $id::new();
            assert_eq!(id.0.get_version_num(), 7);
            assert_ne!(id, $id::new());
            assert_eq!(to_value(id).unwrap(), json!(id.to_string()));
            assert_eq!(from_value::<$id>(to_value(id).unwrap()).unwrap(), id);
        })+};
    }
    check!(
        WorkspaceId,
        SpaceId,
        UserId,
        DotId,
        TeamId,
        ThreadId,
        MessageId,
        JobId,
        TaskId,
        RunId,
        ToolCallRecordId,
        ModelCallId
    );
    assert!(EventSeq(1) < EventSeq(2));
    assert_eq!(to_value(EventSeq(2)).unwrap(), json!(2));
    assert_eq!(LeaseEpoch(3).to_string(), "3");
}

#[test]
fn shared_wire_values_match_json() {
    let statuses = [
        (JobStatus::Accepted, "accepted"),
        (JobStatus::Ready, "ready"),
        (JobStatus::Answered, "answered"),
        (JobStatus::Stopped, "stopped"),
        (JobStatus::NeedsInput, "needs_input"),
        (JobStatus::Cancelled, "cancelled"),
        (JobStatus::Error, "error"),
    ];
    for (value, wire) in statuses {
        assert_eq!(value.wire(), wire);
        assert_eq!(to_value(value).unwrap(), json!(wire));
    }
    for (value, wire) in [
        (HoldReason::DotBusy, "dot_busy"),
        (HoldReason::ProviderQuota, "provider_quota"),
        (HoldReason::WorkspacePaused, "workspace_paused"),
    ] {
        assert_eq!(value.wire(), wire);
        assert_eq!(to_value(value).unwrap(), json!(wire));
    }
    let expected = [
        "provider_unavailable",
        "provider_rate_limited",
        "provider_quota",
        "provider_auth_failed",
        "model_protocol_error",
        "time_limit",
        "quota_exceeded",
        "workspace_paused",
        "step_limit",
        "token_budget",
        "output_truncated",
        "residency_denied",
        "loop_detected",
        "policy_changed",
        "approval_pending",
        "question_pending",
        "interrupted_review_required",
        "dot_paused",
        "retries_exhausted",
        "cancelled",
        "outcome_unknown",
        "no_eligible_dot",
        "dot_not_in_space",
        "lease_lost",
    ];
    assert_eq!(ReasonCode::ALL.len(), expected.len());
    for (value, wire) in ReasonCode::ALL.iter().zip(expected) {
        assert_eq!(value.wire(), wire);
        assert_eq!(to_value(value).unwrap(), json!(wire));
        assert_eq!(from_value::<ReasonCode>(json!(wire)).unwrap(), *value);
    }
    assert_eq!(to_value(ModelTier::Standard).unwrap(), json!("standard"));
    assert_eq!(to_value(Locale::Th).unwrap(), json!("th"));
    assert_eq!(to_value(DotStatus::Paused).unwrap(), json!("paused"));
}

#[test]
fn side_effect_wires_cover_each_outward_kind() {
    let effects = [
        (SideEffect::Read, "read"),
        (SideEffect::Navigate, "navigate"),
        (SideEffect::WorkspaceRead, "workspace_read"),
        (SideEffect::Draft, "draft"),
        (SideEffect::WorkspaceWrite, "workspace_write"),
        (SideEffect::Exec, "exec"),
        (SideEffect::CredentialUse, "credential_use"),
        (SideEffect::Destructive, "destructive"),
        (SideEffect::Outward(OutwardKind::Send), "outward_send"),
        (SideEffect::Outward(OutwardKind::Publish), "outward_publish"),
        (SideEffect::Outward(OutwardKind::Pay), "outward_pay"),
        (SideEffect::Outward(OutwardKind::Share), "outward_share"),
        (
            SideEffect::Outward(OutwardKind::ExternalApi),
            "outward_external_api",
        ),
    ];
    for (value, wire) in effects {
        assert_eq!(value.wire(), wire);
    }
}

#[test]
fn policy_is_explicit_and_question_round_trips_thai() {
    assert!(from_value::<SpacePolicy>(json!({})).is_err());
    assert!(
        !from_value::<SpacePolicy>(json!({"allow_external": false}))
            .unwrap()
            .allow_external
    );
    let question = json!({"question": "เลือกแบบไหน", "options": ["แบบแรก", "แบบสอง"]});
    assert_eq!(
        to_value(from_value::<Question>(question.clone()).unwrap()).unwrap(),
        question
    );
}
