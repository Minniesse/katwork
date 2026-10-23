use axum::{http::StatusCode, response::IntoResponse};
use katwork::error::KatError;
use katwork_server::error::ApiError;

#[tokio::test]
async fn lock_timeout_returns_busy_with_retry_after() {
    let response = ApiError(KatError::Busy).into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!({"code":"busy"})
    );
}

#[tokio::test]
async fn pool_and_connection_errors_during_request_return_503() {
    for error in [
        sqlx::Error::PoolTimedOut,
        sqlx::Error::PoolClosed,
        sqlx::Error::Io(std::io::Error::other("connection lost")),
        sqlx::Error::Tls(Box::new(std::io::Error::other("TLS failed"))),
    ] {
        let error = KatError::from(error);
        assert!(std::error::Error::source(&error).is_some());
        let response = ApiError::from(error).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"code": "database_unavailable"})
        );
    }
}

#[tokio::test]
async fn api_errors_return_only_stable_codes() {
    for (error, status, code) in [
        (
            KatError::ConfigInvalid {
                var: "KATWORK_PUBLIC_URL",
            },
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_invalid",
        ),
        (KatError::UserExists, StatusCode::CONFLICT, "user_exists"),
        (
            KatError::WorkspaceAmbiguous,
            StatusCode::CONFLICT,
            "workspace_ambiguous",
        ),
        (
            KatError::InviteTokenInvalid,
            StatusCode::BAD_REQUEST,
            "invite_token_invalid",
        ),
        (
            KatError::CommandInvalid,
            StatusCode::INTERNAL_SERVER_ERROR,
            "command_invalid",
        ),
        (
            KatError::MigrationRequired,
            StatusCode::INTERNAL_SERVER_ERROR,
            "migration_required",
        ),
        (
            KatError::LinkNotPrinted,
            StatusCode::INTERNAL_SERVER_ERROR,
            "link_not_printed",
        ),
        (
            KatError::DatabaseUnavailable(Box::new(std::io::Error::other(
                uuid::Uuid::now_v7().to_string(),
            ))),
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
        ),
        (
            KatError::from(sqlx::Error::RowNotFound),
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
        ),
        (
            KatError::Migration(sqlx::migrate::MigrateError::VersionMissing(1)),
            StatusCode::INTERNAL_SERVER_ERROR,
            "migration_failed",
        ),
        (
            KatError::Io(std::io::Error::other(uuid::Uuid::now_v7().to_string())),
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
        ),
        (KatError::LeaseLost, StatusCode::CONFLICT, "lease_lost"),
        (
            KatError::JobAlreadyFinished,
            StatusCode::CONFLICT,
            "job_already_finished",
        ),
        (
            KatError::JobNotAwaitingInput,
            StatusCode::CONFLICT,
            "job_not_awaiting_input",
        ),
        (
            KatError::AuthBusy,
            StatusCode::SERVICE_UNAVAILABLE,
            "auth_busy",
        ),
    ] {
        let response = ApiError::from(error).into_response();
        assert_eq!(response.status(), status);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"code": code})
        );
    }
}

#[tokio::test]
async fn rate_limited_sends_retry_after() {
    for seconds in [1, 900, 3600] {
        let response = ApiError::from(KatError::RateLimited {
            retry_after_secs: seconds,
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], seconds.to_string());
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"code": "rate_limited"})
        );
    }
}
