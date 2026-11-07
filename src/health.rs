use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tracing::{info, warn};

#[derive(Default, Clone, Copy, serde::Serialize)]
struct UpdateStatus {
    last_update_success: Option<bool>,
    update_in_progress: bool,
}

pub struct HealthServer {
    update_status: Arc<Mutex<UpdateStatus>>,
}

#[derive(Clone)]
pub struct HealthHandle {
    update_status: Arc<Mutex<UpdateStatus>>,
}

impl HealthServer {
    pub fn new() -> (Self, HealthHandle) {
        let update_status = Arc::new(Mutex::new(UpdateStatus::default()));
        let handle = HealthHandle {
            update_status: update_status.clone(),
        };
        (Self { update_status }, handle)
    }

    pub async fn start(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind("0.0.0.0:8080").await?;
        info!("Health server listening on port 8080");

        loop {
            let (socket, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("Failed to accept connection: {}", e);
                    continue;
                }
            };
            let update_status = self.update_status.clone();

            tokio::spawn(async move {
                let _ = timeout(Duration::from_secs(10), async {
                    handle_connection(socket, update_status).await;
                })
                .await;
            });
        }
    }
}

async fn handle_connection(
    mut socket: tokio::net::TcpStream,
    update_status: Arc<Mutex<UpdateStatus>>,
) {
    let mut buffer = [0; 1024];

    let bytes_read = match timeout(Duration::from_secs(5), socket.read(&mut buffer)).await {
        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
        Ok(Ok(bytes_read)) => bytes_read,
    };
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let path = request.split_whitespace().nth(1).unwrap_or("");
    // Copy the snapshot so the mutex is never held over network I/O.
    let status = update_status.lock().map(|status| *status).ok();

    let (status_line, json_body) = if let Some(status) = status {
        match path {
            // Availability must not depend on an external registry or yesterday's
            // job result. The scheduler continues running after a failed cycle.
            "/" | "/health" => ("HTTP/1.1 200 OK", r#"{"status":"healthy"}"#.to_string()),
            "/updates" => {
                let status_line = if status.last_update_success == Some(false) {
                    "HTTP/1.1 503 Service Unavailable"
                } else {
                    "HTTP/1.1 200 OK"
                };
                (status_line, serde_json::to_string(&status).unwrap())
            }
            _ => (
                "HTTP/1.1 404 Not Found",
                r#"{"status":"not_found"}"#.to_string(),
            ),
        }
    } else {
        (
            "HTTP/1.1 503 Service Unavailable",
            r#"{"status":"unhealthy"}"#.to_string(),
        )
    };

    let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json_body}",
        json_body.len(),
    );

    let _ = timeout(
        Duration::from_secs(5),
        socket.write_all(response.as_bytes()),
    )
    .await;
}

impl HealthHandle {
    pub fn report_update_started(&self) {
        if let Ok(mut status) = self.update_status.lock() {
            status.update_in_progress = true;
        }
    }

    /// Record the last completed cycle, independently of service availability.
    pub fn set_health_status(&self, is_healthy: bool) {
        if let Ok(mut status) = self.update_status.lock() {
            status.last_update_success = Some(is_healthy);
            status.update_in_progress = false;
        }
    }

    pub fn report_update_success(&self) {
        self.set_health_status(true);
    }

    pub fn report_update_failure(&self) {
        self.set_health_status(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise actual HTTP responses without competing for the production port.
    async fn request(server: &HealthServer, path: &str) -> (String, serde_json::Value) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let handler = tokio::spawn(handle_connection(socket, server.update_status.clone()));
        client
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        timeout(Duration::from_secs(2), client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        handler.await.unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.contains(&format!("Content-Length: {}\r\n", body.len())));
        (
            headers.lines().next().unwrap().to_string(),
            serde_json::from_str(body).unwrap(),
        )
    }

    #[tokio::test]
    async fn availability_survives_failed_updates_and_recovers_update_status() {
        let (server, handle) = HealthServer::new();
        let (_, body) = request(&server, "/updates").await;
        assert!(body["last_update_success"].is_null());
        assert_eq!(body["update_in_progress"], false);

        handle.report_update_started();
        assert_eq!(request(&server, "/health").await.0, "HTTP/1.1 200 OK");
        let (_, body) = request(&server, "/updates").await;
        assert!(body["last_update_success"].is_null());
        assert_eq!(body["update_in_progress"], true);

        handle.report_update_failure();
        assert_eq!(request(&server, "/health").await.0, "HTTP/1.1 200 OK");
        let (status, body) = request(&server, "/updates").await;
        assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
        assert_eq!(body["last_update_success"], false);
        assert_eq!(body["update_in_progress"], false);

        // Starting another cycle must not erase an outstanding failure.
        handle.report_update_started();
        let (status, body) = request(&server, "/updates").await;
        assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
        assert_eq!(body["update_in_progress"], true);
        assert_eq!(request(&server, "/").await.0, "HTTP/1.1 200 OK");

        handle.report_update_success();
        let (status, body) = request(&server, "/updates").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body["last_update_success"], true);
        assert_eq!(body["update_in_progress"], false);
        assert_eq!(
            request(&server, "/missing").await.0,
            "HTTP/1.1 404 Not Found"
        );
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
        let (server, handle) = HealthServer::new();
        let scheduler = Scheduler::new(config, Some(handle)).unwrap();

        assert!(scheduler.run_once().await.is_err());
        assert_eq!(request(&server, "/health").await.0, "HTTP/1.1 200 OK");
        assert_eq!(
            request(&server, "/updates").await.0,
            "HTTP/1.1 503 Service Unavailable"
        );

        std::fs::write(
            directory.path().join("valid.yml"),
            "services:\n  app:\n    image: example:latest\n",
        )
        .unwrap();
        let broken = directory.path().join("broken.yml");
        std::fs::write(&broken, "services: [invalid yaml").unwrap();
        assert!(scheduler.run_once().await.is_err());
        assert_eq!(request(&server, "/health").await.0, "HTTP/1.1 200 OK");
        assert_eq!(
            request(&server, "/updates").await.0,
            "HTTP/1.1 503 Service Unavailable"
        );

        std::fs::remove_file(broken).unwrap();
        scheduler.run_once().await.unwrap();
        let (status, body) = request(&server, "/updates").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body["last_update_success"], true);
        assert_eq!(body["update_in_progress"], false);
    }
}
