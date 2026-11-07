use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tracing::info;

#[derive(Default, Clone, Copy, serde::Serialize)]
struct UpdateStatus {
    last_update_success: Option<bool>,
    update_in_progress: bool,
}

#[derive(Clone, Default)]
pub struct Health {
    update_status: Arc<Mutex<UpdateStatus>>,
}

impl Health {
    pub async fn start(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind("0.0.0.0:8080").await?;
        info!("Health server listening on port 8080");
        axum::serve(listener, self.router()).await?;
        Ok(())
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/", get(availability))
            .route("/health", get(availability))
            .route("/updates", get(update_status))
            .with_state(self.clone())
    }

    pub fn update_started(&self) {
        self.update_status.lock().unwrap().update_in_progress = true;
    }

    pub fn update_finished(&self, success: bool) {
        *self.update_status.lock().unwrap() = UpdateStatus {
            last_update_success: Some(success),
            update_in_progress: false,
        };
    }
}

async fn availability() -> Json<Value> {
    Json(json!({"status": "healthy"}))
}

async fn update_status(State(health): State<Health>) -> (StatusCode, Json<UpdateStatus>) {
    let status = *health.update_status.lock().unwrap();
    let code = if status.last_update_success == Some(false) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (code, Json(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        time::timeout,
    };
    use tower::ServiceExt;

    async fn request(health: &Health, path: &str) -> (StatusCode, Value) {
        let response = health
            .router()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let json = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
        (status, json)
    }

    #[tokio::test]
    async fn serves_fragmented_http_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Health::default().router();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"GET /hea").await.unwrap();
        assert!(timeout(Duration::from_millis(25), client.read(&mut [0]))
            .await
            .is_err());
        client
            .write_all(b"lth HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        timeout(Duration::from_secs(2), client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains(r#"{"status":"healthy"}"#));
        server.abort();
    }

    #[tokio::test]
    async fn availability_survives_failed_updates_and_recovers_update_status() {
        let health = Health::default();
        let (_, body) = request(&health, "/updates").await;
        assert!(body["last_update_success"].is_null());
        assert_eq!(body["update_in_progress"], false);

        health.update_started();
        assert_eq!(request(&health, "/health").await.0, StatusCode::OK);
        let (_, body) = request(&health, "/updates").await;
        assert!(body["last_update_success"].is_null());
        assert_eq!(body["update_in_progress"], true);

        health.update_finished(false);
        assert_eq!(request(&health, "/health").await.0, StatusCode::OK);
        let (status, body) = request(&health, "/updates").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["last_update_success"], false);
        assert_eq!(body["update_in_progress"], false);

        // Starting another cycle must not erase an outstanding failure.
        health.update_started();
        let (status, body) = request(&health, "/updates").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["update_in_progress"], true);
        assert_eq!(request(&health, "/").await.0, StatusCode::OK);

        health.update_finished(true);
        let (status, body) = request(&health, "/updates").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["last_update_success"], true);
        assert_eq!(body["update_in_progress"], false);
        assert_eq!(request(&health, "/missing").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn scheduler_reports_empty_partial_failure_and_success_over_http() {
        use crate::{config::Config, scheduler::Scheduler};

        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            compose_paths: vec![directory.path().to_path_buf()],
            dry_run: true,
            ..Config::default()
        };
        let health = Health::default();
        let scheduler = Scheduler::new(config, Some(health.clone())).unwrap();

        assert!(scheduler.run_once().await.is_err());
        assert_eq!(request(&health, "/health").await.0, StatusCode::OK);
        assert_eq!(
            request(&health, "/updates").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );

        std::fs::write(
            directory.path().join("valid.yml"),
            "services:\n  app:\n    image: example:latest\n",
        )
        .unwrap();
        let broken = directory.path().join("broken.yml");
        std::fs::write(&broken, "services: [invalid yaml").unwrap();
        assert!(scheduler.run_once().await.is_err());
        assert_eq!(request(&health, "/health").await.0, StatusCode::OK);
        assert_eq!(
            request(&health, "/updates").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );

        std::fs::remove_file(broken).unwrap();
        scheduler.run_once().await.unwrap();
        let (status, body) = request(&health, "/updates").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["last_update_success"], true);
        assert_eq!(body["update_in_progress"], false);
    }
}
