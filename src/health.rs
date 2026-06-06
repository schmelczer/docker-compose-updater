use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tracing::{info, warn};

pub struct HealthServer {
    last_update_success: Arc<Mutex<bool>>,
}

#[derive(Clone)]
pub struct HealthHandle {
    last_update_success: Arc<Mutex<bool>>,
}

impl HealthServer {
    pub fn new() -> (Self, HealthHandle) {
        let last_update_success = Arc::new(Mutex::new(true));
        let handle = HealthHandle {
            last_update_success: last_update_success.clone(),
        };
        (
            Self {
                last_update_success,
            },
            handle,
        )
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
            let health_status = self.last_update_success.clone();

            tokio::spawn(async move {
                let _ = timeout(Duration::from_secs(10), async {
                    handle_connection(socket, health_status).await;
                })
                .await;
            });
        }
    }
}

async fn handle_connection(mut socket: tokio::net::TcpStream, health_status: Arc<Mutex<bool>>) {
    let mut buffer = [0; 1024];

    match timeout(Duration::from_secs(5), socket.read(&mut buffer)).await {
        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
        Ok(Ok(_)) => {}
    }

    let is_healthy = health_status.lock().map(|s| *s).unwrap_or(false);

    let (status_line, json_body) = if is_healthy {
        ("HTTP/1.1 200 OK", r#"{"status":"healthy"}"#)
    } else {
        (
            "HTTP/1.1 503 Service Unavailable",
            r#"{"status":"unhealthy"}"#,
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
    pub fn set_health_status(&self, is_healthy: bool) {
        if let Ok(mut status) = self.last_update_success.lock() {
            *status = is_healthy;
        }
    }

    pub fn report_update_success(&self) {
        self.set_health_status(true);
    }

    pub fn report_update_failure(&self) {
        self.set_health_status(false);
    }
}
