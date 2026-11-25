//! Files of a Space: what a person uploads (inputs) and what a Kat writes (outputs), their
//! versions, and the blobs that hold their bytes.
//!
//! Blobs are content-addressed under one directory per workspace (`blob`), so addressing never
//! crosses tenants. A blob is committed only under the blob lock and inside the transaction that
//! inserts the version naming it (`store::commit_blob_on`), and every artifact row is locked by
//! the artifact lock contract's primitives (`store::lock_source_on`, `store::lock_artifacts_on`).
use serde::{Deserialize, Serialize};

pub mod blob;
pub mod media;
pub mod names;
pub mod store;
pub mod uploads;

pub use blob::{BlobRef, BlobStore, BlobStream, LocalBlobStore, StagedBlob, Staging};

/// A file's type, decided at upload from its name and its first bytes (`media::sniff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum MediaType {
    Pdf,
    Docx,
    Xlsx,
    Text,
    Markdown,
    Csv,
    Png,
    Jpeg,
}

impl MediaType {
    pub const ALL: [Self; 8] = [
        Self::Pdf,
        Self::Docx,
        Self::Xlsx,
        Self::Text,
        Self::Markdown,
        Self::Csv,
        Self::Png,
        Self::Jpeg,
    ];

    pub fn wire(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Csv => "csv",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }

    pub fn from_wire(wire: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|media| media.wire() == wire)
    }

    /// The `Content-Type` of a download. Text types carry no `charset`: a UTF-16 file is kept as
    /// it was uploaded.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Text => "text/plain",
            Self::Markdown => "text/markdown",
            Self::Csv => "text/csv",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ArtifactRole {
    /// Uploaded by a person; never versioned.
    Input,
    /// Written by a Kat in a job; versioned by later writes.
    Output,
}

impl ArtifactRole {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// Whether GridCraft checked an XLSX output before it was committed (R19). Null for every input
/// and every other type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum GridcraftCheck {
    Passed,
    Unavailable,
    Off,
}

impl GridcraftCheck {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Unavailable => "unavailable",
            Self::Off => "off",
        }
    }
}
