//! A Space's files: upload one, list them, download a version.
use std::borrow::Cow;

use axum::{
    Json,
    body::Body,
    extract::{Path, RawQuery, Request, State, rejection::PathRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use katwork::{
    artifacts::{
        BlobRef, media, names,
        store::{self, ArtifactView},
        uploads::{self, RateCheck},
    },
    db,
    error::KatError,
    ids::{ArtifactId, SpaceId},
    runtime::Core,
    tenancy::{Tenant, space},
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use sqlx::{Connection, PgConnection};

use crate::{app::AppState, error::ApiError, extract::path as route_path};

/// The upload route. It sits outside `guards::bounded` (no 1 MiB body, no 30 s timeout), and
/// the guard accepts `application/octet-stream`, and only that, on `POST` here.
pub(crate) const UPLOAD_ROUTE: &str = "/api/workspaces/{workspace_id}/spaces/{space_id}/files";
pub(crate) const CONTENT_ROUTE: &str =
    "/api/workspaces/{workspace_id}/files/{artifact_id}/versions/{version}/content";
/// The most files one listing returns.
const LIST_LIMIT: u32 = 500;
/// A download is a sandboxed attachment, never a page.
const DOWNLOAD_CSP: &str = "default-src 'none'; sandbox";

/// RFC 8187 `attr-char`; every other byte of a `filename*` value is percent-encoded.
const ATTR_CHAR: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

#[derive(Deserialize)]
pub(crate) struct SpacePath {
    space_id: SpaceId,
}

#[derive(Deserialize)]
pub(crate) struct ContentPath {
    artifact_id: ArtifactId,
    version: i32,
}

#[derive(Serialize)]
pub(crate) struct FileList {
    files: Vec<ArtifactView>,
}

/// `POST …/files?name=<path>` with the file as an `application/octet-stream` body: membership
/// (the extractor), the name, a declared length over the cap (before any body byte), the Space,
/// the limiter and the room for it, then the body under the rate floor and the upload timeout,
/// the type, and one transaction for the rows and the blob. 201 with the file.
///
/// Once the body is in, the commit runs in a task of its own that holds the upload's slot, so a
/// client that leaves then gets a 201 nobody reads, never a blob without its rows.
pub(crate) async fn upload(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
    request: Request,
) -> Result<(StatusCode, Json<ArtifactView>), ApiError> {
    let space_id = route_path(path)?.space_id;
    let core = &state.core;
    let name = query_param(request.uri().query(), "name")?.ok_or(KatError::FileNameInvalid)?;
    let name = names::validate_name(&name)?;
    let cap = core.config.upload_max_bytes;
    let declared = content_length(request.headers());
    if declared.is_some_and(|length| length > cap) {
        return Err(KatError::BodyTooLarge.into());
    }
    read_bounded(core, async |conn| {
        space::get_on(conn, &tenant, space_id).await
    })
    .await?;
    // Held to the end of the commit, so every way out frees it.
    let permit = core.uploads.try_acquire(tenant.user_id())?;
    let free = core.blobs.free_bytes().await?;
    let used = read_bounded(core, async |conn| {
        store::workspace_bytes_on(conn, tenant.workspace_id()).await
    })
    .await?;
    uploads::check_space(
        free,
        used,
        declared.unwrap_or(cap),
        core.config.workspace_storage_bytes,
    )?;
    let mut staging = core.blobs.stage(tenant.workspace_id(), cap).await?;
    let mut rate = RateCheck::new(
        core.config.upload_min_bytes_per_sec,
        core.config.upload_rate_window,
    );
    uploads::stream_body(
        request.into_body().into_data_stream(),
        &mut staging,
        &mut rate,
        uploads::UPLOAD_TIMEOUT,
    )
    .await?;
    let core = state.core.clone();
    let commit = tokio::spawn(async move {
        let _permit = permit;
        let staged = staging.finish().await?;
        let media = media::sniff(&name, &staged.head, staged.text)?;
        store::create_input(
            &core.pool,
            core.blobs.as_ref(),
            &tenant,
            space_id,
            &name,
            media,
            staged,
            core.config.workspace_storage_bytes,
            core.config.lock_timeout,
        )
        .await
    });
    let view = commit
        .await
        .map_err(|error| KatError::Io(std::io::Error::other(error)))??;
    Ok((StatusCode::CREATED, Json(view)))
}

/// A read before the body, in a guarded transaction of its own under the lock timeout, so an
/// upload never waits on a lock longer than that, with or without its slot.
async fn read_bounded<T>(
    core: &Core,
    read: impl AsyncFnOnce(&mut PgConnection) -> Result<T, KatError>,
) -> Result<T, KatError> {
    db::guarded(&core.pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, core.config.lock_timeout).await?;
        let value = read(&mut tx).await?;
        tx.commit().await?;
        Ok(value)
    })
    .await
}

/// `GET …/spaces/{space_id}/files?prefix=<p>`: the Space's files, by name, at most 500.
pub(crate) async fn list(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
    RawQuery(query): RawQuery,
) -> Result<Json<FileList>, ApiError> {
    let space_id = route_path(path)?.space_id;
    let prefix = query_param(query.as_deref(), "prefix")?;
    let files = store::list(
        &state.core.pool,
        &tenant,
        space_id,
        prefix.as_deref(),
        LIST_LIMIT,
    )
    .await?;
    Ok(Json(FileList { files }))
}

/// `GET …/files/{artifact_id}/versions/{version}/content`: the version's bytes as an
/// attachment. The blob is hashed before the first byte is sent.
pub(crate) async fn download(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<ContentPath>, PathRejection>,
) -> Result<Response, ApiError> {
    let ContentPath {
        artifact_id,
        version,
    } = route_path(path)?;
    let (artifact, version) =
        store::version(&state.core.pool, &tenant, artifact_id, version).await?;
    let blob = BlobRef {
        workspace_id: tenant.workspace_id(),
        sha256: version.sha256,
        bytes: u64::try_from(version.bytes).map_err(|_| KatError::BlobCorrupt)?,
    };
    let body = state.core.blobs.open(&blob).await?;
    let file_name = artifact.name.rsplit('/').next().unwrap_or_default();
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static(version.media_type.mime()),
        ),
        (header::CONTENT_LENGTH, HeaderValue::from(blob.bytes)),
        (header::CONTENT_DISPOSITION, content_disposition(file_name)?),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        ),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(DOWNLOAD_CSP),
        ),
    ];
    Ok((headers, Body::from_stream(body)).into_response())
}

/// `attachment; filename*=UTF-8''<name>`, the name percent-encoded as RFC 8187 requires, so the
/// header is visible ASCII whatever the script of the name.
fn content_disposition(file_name: &str) -> Result<HeaderValue, KatError> {
    HeaderValue::try_from(format!(
        "attachment; filename*=UTF-8''{}",
        utf8_percent_encode(file_name, ATTR_CHAR)
    ))
    .map_err(|_| KatError::FileNameInvalid)
}

/// The value of the one `key` parameter of a form-encoded query: `+` is a space, as
/// `URLSearchParams` writes it, and the decoded bytes must be UTF-8. A repeated parameter, or one
/// that is not UTF-8 once decoded, is `FileNameInvalid`, never a lossy guess. Other parameters
/// are ignored.
fn query_param(query: Option<&str>, key: &str) -> Result<Option<String>, KatError> {
    let mut found = None;
    for pair in query.unwrap_or_default().split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if form_decode_lossy(name) != key {
            continue;
        }
        if found.is_some() {
            return Err(KatError::FileNameInvalid);
        }
        found = Some(form_decode(value)?);
    }
    Ok(found)
}

fn form_decode(raw: &str) -> Result<String, KatError> {
    percent_decode_str(&raw.replace('+', " "))
        .decode_utf8()
        .map(Cow::into_owned)
        .map_err(|_| KatError::FileNameInvalid)
}

fn form_decode_lossy(raw: &str) -> String {
    percent_decode_str(&raw.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// A single, numeric `Content-Length`; anything else is treated as no length (the staging cap
/// still holds).
fn content_length(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    value.trim().parse().ok()
}
