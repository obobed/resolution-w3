use axum::{
    Json, Router, extract::{Path, State}, http::StatusCode, response::Redirect, routing::{get, post}
};

use chrono::{DateTime, Utc};
use nanoid::nanoid;

use serde::{Deserialize, Serialize};
use sqlx::{prelude::FromRow, sqlite::SqlitePool};
use std::sync::Arc;

use std::net::SocketAddr;
use tower_governor::{
    GovernorLayer, governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor,
};

use utoipa::{OpenApi, ToSchema};
use utoipa_scalar::{Scalar, Servable};
struct AppState {
    db: SqlitePool,
    config: AppConfig,
}

struct AppConfig {
    max_paste_size: usize,
}

#[derive(Serialize, FromRow, ToSchema)]
struct Paste {
    id: String,
    content: String,
    created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
struct CreatePaste {
    content: String,
}

#[derive(Serialize, ToSchema)]
struct ApiResponse {
    contents: String,
}

#[derive(OpenApi)]
#[openapi(
    paths(create_paste, get_paste, list_pastes, health),
    components(schemas(Paste, CreatePaste, ApiResponse))
)]
struct ApiDoc;

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Ok")
    )
)]
async fn health() -> Json<ApiResponse> {
    Json(ApiResponse {
        contents: "ok".to_string(),
    })
}

#[utoipa::path(
    post,
    path = "/pastes/new",
    request_body = CreatePaste,
    responses(
        (status = 201, description = "Paste created successfully", body = Paste),
        (status = 429, description = "Too many requests")
    )
)]
async fn create_paste(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreatePaste>,
) -> Result<(StatusCode, Json<Paste>), (StatusCode, Json<ApiResponse>)> {
    if body.content.len() > state.config.max_paste_size {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(ApiResponse {
                contents: "Too large!".into(),
            }),
        ));
    }

    let id = nanoid!(10);

    let now = Utc::now();

    sqlx::query("INSERT INTO pastes (id, content, created_at) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&body.content)
        .bind(now)
        .execute(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    contents: format!("database error!: {e}"),
                }),
            )
        })?;

    let new_paste = Paste {
        id,
        content: body.content,
        created_at: now,
    };

    Ok((StatusCode::CREATED, Json(new_paste)))
}

#[utoipa::path(
    get,
    path = "/pastes",
    responses(
        (status = 200, description = "List of the 50 most recent pastes", body = [Paste]),
        (status = 500, description = "Internal database error", body = ApiResponse)
    )
)]
async fn list_pastes(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Paste>>, (StatusCode, Json<ApiResponse>)> {
    let pastes: Vec<Paste> = sqlx::query_as(
        "SELECT id, content, created_at FROM pastes ORDER BY created_at DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                contents: format!("database error!: {e}"),
            }),
        )
    })?;

    Ok(Json(pastes))
}

#[utoipa::path(
    get,
    path = "/pastes/{id}",
    responses(
        (status = 200, description = "Paste found", body = Paste),
        (status = 404, description = "Paste not found", body = ApiResponse),
        (status = 500, description = "Internal server error", body = ApiResponse)
    ),
    params(
        ("id" = String, Path, description = "The unique ID of the paste")
    )
)]
async fn get_paste(
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Paste>, (StatusCode, Json<ApiResponse>)> {
    let paste: Option<Paste> =
        sqlx::query_as("SELECT id, content, created_at FROM pastes WHERE id = ? LIMIT 1")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse {
                        contents: format!("database error!: {e}"),
                    }),
                )
            })?;
    match paste {
        Some(p) => Ok(Json(p)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                contents: "Paste not found".into(),
            }),
        )),
    }
}

async fn handler_404() -> (StatusCode, Json<ApiResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiResponse {
            contents: "Route not found, check your URL!".to_string()
        }),
    )
}

async fn gh_redirect() -> Redirect {
    Redirect::permanent("https://github.com/obobed/resolution-w3")
}

async fn root_redirect() -> Redirect {
    Redirect::permanent("/docs")
}

#[tokio::main]
async fn main() {
    let db = SqlitePool::connect("sqlite:hastebin.db?mode=rwc")
        .await
        .expect("Failed to connect to a database");

    let config = AppConfig {
        max_paste_size: 3200,
    };

    let create_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(5)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap(),
    );

    let read_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(10)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap(),
    );

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pastes (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            created_at DATETIME NOT NULL
        )",
    )
    .execute(&db)
    .await
    .expect("failed to create table");

    let state = Arc::new(AppState { db, config });

    let app = Router::new()
        .merge(Scalar::with_url("/docs", ApiDoc::openapi()))
        .route("/health", get(health))
        .route(
            "/pastes/new",
            post(create_paste).layer(GovernorLayer::new(create_conf)),
        )
        .route(
            "/pastes/{id}",
            get(get_paste).layer(GovernorLayer::new(read_conf.clone())),
        )
        .route(
            "/pastes",
            get(list_pastes).layer(GovernorLayer::new(read_conf)),
        )
        .route(
            "/gh",
            get(gh_redirect)
        )
        .route(
            "/",
            get(root_redirect)
        )
        .fallback(handler_404)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:5417")
        .await
        .expect("Failed to bind");

    println!("listening on http://0.0.0.0:5417");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
