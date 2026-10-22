use axum::{http::StatusCode, response::IntoResponse};
use katwork::error::KatError;
use katwork_server::error::ApiError;

#[tokio::test]
async fn api_errors_return_only_stable_codes() {
    for (error, status, code) in [
        (
            KatError::ConfigInvalid,
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_invalid",
        ),
        (
            KatError::CommandInvalid,
            StatusCode::INTERNAL_SERVER_ERROR,
            "command_invalid",
        ),
        (
            KatError::DatabaseUnavailable(Box::new(std::io::Error::other(
                uuid::Uuid::now_v7().to_string(),
            ))),
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
        ),
        (
            KatError::Database(sqlx::Error::RowNotFound),
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
