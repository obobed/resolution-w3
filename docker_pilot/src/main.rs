use axum::{
    Json, Router,
    extract::{State, Path},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use bollard::Docker;
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptions, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use std::sync::Arc;

struct AppState {
    docker: Docker,
    db: SqlitePool,
    api_key: String,
}

#[derive(Serialize)]
struct ContainerInfo {
    id: String,
    name: String,
    image: String,
    state: String,
}
    
#[derive(Deserialize)]
struct CreateContainerRequest {
    name: String,
    image: String,
}

#[derive(Serialize)]
struct ApiResponse {
    message: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct AuditLog {
    id: i64,
    action: String,
    container_name: String,
    timestamp: String,
}

fn check_auth(headers: &HeaderMap, api_key: &str) -> Result<(), (StatusCode, Json<ApiResponse>)> {
    let provided = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if provided != api_key {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse {
                message: "invalid or missing API key".to_string(),
            }),
        ));
    }

    Ok(())
}

async fn log_action(db: &SqlitePool, action: &str, container_name: &str) {
    let _ = sqlx::query("INSERT INTO audit_log (action, container_name) VALUES (?, ?)")
        .bind(action)
        .bind(container_name)
        .execute(db)
        .await;
}

async fn health() -> Json<ApiResponse> {
    Json(ApiResponse {
        message: "ok".to_string(),
    })
}

async fn list_containers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ContainerInfo>>, (StatusCode, Json<ApiResponse>)> {
    check_auth(&headers, &state.api_key)?;

    let options = ListContainersOptions {
        all: true,
        ..Default::default()
    };

    let containers = state
        .docker
        .list_containers(Some(options))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    message: format!("Docker error: {e}"),
                }),
            )
        })?;

    let result: Vec<ContainerInfo> = containers
        .into_iter()
        .map(|c| ContainerInfo {
            id: c.id.unwrap_or_default(),
            name: c
                .names
                .and_then(|n| n.first().cloned())
                .unwrap_or_default()
                .trim_start_matches('/')
                .to_string(),
            image: c.image.unwrap_or_default(),
            state: c
                .state
                .map(|s| s.to_string())
                .unwrap_or_default(),
        })
        .collect();

    Ok(Json(result))
}

async fn create_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateContainerRequest>,
) -> Result<(StatusCode, Json<ApiResponse>), (StatusCode, Json<ApiResponse>)> {
    check_auth(&headers, &state.api_key)?;

    let options = CreateContainerOptionsBuilder::new()
        .name(&body.name)
        .build();

    let config = ContainerCreateBody {
        image: Some(body.image),
        ..Default::default()
    };

    state
        .docker
        .create_container(Some(options), config)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    message: format!("failed to create container: {e}"),
                }),
            )
        })?;

    state
        .docker
        .start_container(&body.name, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    message: format!("created but failed to start: {e}"),
                }),
            )
        })?;

    log_action(&state.db, "create", &body.name).await;

    Ok((
        StatusCode::CREATED,
        Json(ApiResponse {
            message: format!("container '{}' created and started", body.name),
        }),
    ))
}

async fn stop_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse>, (StatusCode, Json<ApiResponse>)> {
    check_auth(&headers, &state.api_key)?;

    let options = StopContainerOptionsBuilder::new()
        .t(10)
        .build();

    state
        .docker
        .stop_container(&name, Some(options))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    message: format!("failed to stop container: {e}"),
                }),
            )
        })?;

    log_action(&state.db, "stop", &name).await;

    Ok(Json(ApiResponse {
        message: format!("container '{name}' stopped"),
    }))
}

async fn remove_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<ApiResponse>, (StatusCode, Json<ApiResponse>)> {
    check_auth(&headers, &state.api_key)?;

    let options = RemoveContainerOptionsBuilder::new()
        .force(true)
        .build();

    state
        .docker
        .remove_container(&name, Some(options))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    message: format!("failed to remove container: {e}"),
                }),
            )
        })?;

    log_action(&state.db, "remove", &name).await;

    Ok(Json(ApiResponse {
        message: format!("container '{name}' removed"),
    }))
}

async fn get_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<AuditLog>>, (StatusCode, Json<ApiResponse>)> {
    check_auth(&headers, &state.api_key)?;

    let rows: Vec<AuditLog> = sqlx::query_as(
        "SELECT id, action, container_name, timestamp FROM audit_log ORDER BY id DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                message: format!("database error: {e}"),
            }),
        )
    })?;

    Ok(Json(rows))
}

#[tokio::main]
async fn main() {
    let api_key = std::env::var("API_KEY").expect("API_KEY must be set");

    let db = SqlitePool::connect("sqlite:docker_pilot.db?mode=rwc")
        .await
        .expect("failed to connect to database");

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            action TEXT NOT NULL,
            container_name TEXT NOT NULL,
            timestamp TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(&db)
    .await
    .expect("failed to create table");

    let docker = Docker::connect_with_local_defaults().expect("failed to connect to Docker");

    let state = Arc::new(AppState {
        docker,
        db,
        api_key,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/containers", get(list_containers))
        .route("/containers", post(create_container))
        .route("/containers/{name}/stop", post(stop_container))
        .route("/containers/{name}", delete(remove_container))
        .route("/logs", get(get_logs))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind");

    println!("listening on http://0.0.0.0:3000");
    axum::serve(listener, app).await.expect("server error");
}