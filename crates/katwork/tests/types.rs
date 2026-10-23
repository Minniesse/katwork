use katwork::{ids::*, types::*};
use serde_json::{from_value, json, to_value};

#[test]
fn name_unicode_repertoire_does_not_exceed_case_fold_tables() {
    assert!(unicode_properties::UNICODE_VERSION <= caseless::UNICODE_VERSION);
}

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
        KatId,
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
        assert_eq!(from_value::<JobStatus>(json!(wire)).unwrap(), value);
    }
    for (value, wire) in [
        (HoldReason::KatBusy, "kat_busy"),
        (HoldReason::ProviderQuota, "provider_quota"),
        (HoldReason::WorkspacePaused, "workspace_paused"),
    ] {
        assert_eq!(value.wire(), wire);
        assert_eq!(to_value(value).unwrap(), json!(wire));
        assert_eq!(from_value::<HoldReason>(json!(wire)).unwrap(), value);
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
        "kat_paused",
        "internal_error",
        "retries_exhausted",
        "cancelled",
        "outcome_unknown",
        "no_eligible_kat",
        "kat_not_in_space",
        "lease_lost",
        "too_many_requests",
        "workspace_paused_stopped",
        "kat_paused_stopped",
        "mention_needs_space",
        "extra_mention_ignored",
    ];
    assert_eq!(ReasonCode::ALL.len(), expected.len());
    for (value, wire) in ReasonCode::ALL.iter().zip(expected) {
        assert_eq!(value.wire(), wire);
        assert_eq!(to_value(value).unwrap(), json!(wire));
        assert_eq!(from_value::<ReasonCode>(json!(wire)).unwrap(), *value);
    }
    for invalid in [json!("unknown"), json!(null), json!(1)] {
        assert!(from_value::<JobStatus>(invalid.clone()).is_err());
        assert!(from_value::<HoldReason>(invalid.clone()).is_err());
        assert!(from_value::<ReasonCode>(invalid).is_err());
    }
}

#[test]
fn shared_type_wire_matrix_round_trips_every_variant() {
    macro_rules! variants {
        ($kind:ident, $($variant:ident => $wire:literal),+ $(,)?) => {{
            let expected = |value: $kind| match value {
                $($kind::$variant => $wire),+
            };
            $(
                assert_eq!(to_value($kind::$variant).unwrap(), json!(expected($kind::$variant)));
                assert_eq!(from_value::<$kind>(json!($wire)).unwrap(), $kind::$variant);
            )+
            for invalid in [json!("unknown"), json!(null), json!(1)] {
                assert!(from_value::<$kind>(invalid).is_err());
            }
        }};
    }
    variants!(ModelTier, Fast => "fast", Standard => "standard", Strong => "strong");
    variants!(Locale, En => "en", Th => "th");
    variants!(KatStatus, Active => "active", Paused => "paused");
    variants!(OutwardKind, Send => "send", Publish => "publish", Pay => "pay",
        Share => "share", ExternalApi => "external_api");

    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(to_value(EventSeq(value)).unwrap(), json!(value));
        assert_eq!(
            from_value::<EventSeq>(json!(value)).unwrap(),
            EventSeq(value)
        );
        assert_eq!(EventSeq(value).to_string(), value.to_string());
        assert_eq!(to_value(LeaseEpoch(value)).unwrap(), json!(value));
        assert_eq!(
            from_value::<LeaseEpoch>(json!(value)).unwrap(),
            LeaseEpoch(value)
        );
        assert_eq!(LeaseEpoch(value).to_string(), value.to_string());
    }
    for invalid in [json!("1"), json!(null), json!(1.5), json!(u64::MAX)] {
        assert!(from_value::<EventSeq>(invalid.clone()).is_err());
        assert!(from_value::<LeaseEpoch>(invalid).is_err());
    }
    assert!(LeaseEpoch(1) < LeaseEpoch(2));
}

#[test]
fn side_effect_wire_strings_round_trip_every_variant() {
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
        assert_eq!(SideEffect::from_wire(wire), Some(value));
        assert_eq!(to_value(value).unwrap(), json!(wire));
        assert_eq!(from_value::<SideEffect>(json!(wire)).unwrap(), value);
    }
    for invalid in [json!("shell"), json!({"outward": "send"}), json!(null)] {
        assert!(from_value::<SideEffect>(invalid).is_err());
    }
    assert_eq!(SideEffect::from_wire("shell"), None);
    assert_eq!(SideEffect::from_wire("outward"), None);
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
