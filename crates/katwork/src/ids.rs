use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Ids order bytewise, as `Uuid` does and as PostgreSQL orders `uuid` values, so a sort in Rust
// matches `ORDER BY id` (the artifact lock contract locks rows in ascending id).
macro_rules! typed_id {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )+};
}

typed_id!(
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
    ModelCallId,
    ArtifactId,
    ArtifactVersionId
);

macro_rules! counter {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, sqlx::Type)]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )+};
}

counter!(EventSeq, LeaseEpoch);
