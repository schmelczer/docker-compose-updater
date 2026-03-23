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

impl Default for HealthServer {
    fn default() -> Self {
        Self::new().0
    }
}

impl HealthServer {
    pub fn new() -> (Self, HealthHandle) {
        let last_update_success = Arc::new(Mutex::new(true));
        let server = Self {
            last_update_success: last_update_success.clone(),
        };
        let handle = HealthHandle {
            last_update_success,
        };
        (server, handle)
    }

    pub async fn start(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind("0.0.0.0:8080").await?;
        info!("Health server listening on port 8080");

        loop {
            let (mut socket, _) = listener.accept().await?;
            let health_status = self.last_update_success.clone();

            tokio::spawn(async move {
                // Set timeout for the entire request handling (5 seconds)
                let handle_request = async {
                    let mut buffer = [0; 1024];

                    // Read with timeout to prevent hanging connections
                    match timeout(Duration::from_secs(5), socket.read(&mut buffer)).await {
                        Ok(Ok(_)) => {
                            // Successfully read request (we don't need to parse it for health check)
                        }
                        Ok(Err(_)) | Err(_) => {
                            warn!("Health check request read timeout or error");
                            return;
                        }
                    }

                    // Safely access health status without panicking
                    let is_healthy = match health_status.lock() {
                        Ok(status) => *status,
                        Err(_) => {
                            warn!("Health status mutex poisoned, defaulting to unhealthy");
                            false
                        }
                    };

                    let (status_line, json_body) = if is_healthy {
                        ("HTTP/1.1 200 OK", "{\"status\":\"healthy\"}")
                    } else {
                        (
                            "HTTP/1.1 503 Service Unavailable",
                            "{\"status\":\"unhealthy\"}",
                        )
                    };

                    let response = format!(
                        "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status_line,
                        json_body.len(),
                        json_body
                    );

                    let _ = timeout(
                        Duration::from_secs(5),
                        socket.write_all(response.as_bytes()),
                    )
                    .await;
                };

                // Overall timeout for the entire connection handling
                let _ = timeout(Duration::from_secs(10), handle_request).await;
            });
        }
    }
}

impl HealthHandle {
    pub fn set_health_status(&self, is_healthy: bool) {
        if let Ok(mut status) = self.last_update_success.lock() {
            *status = is_healthy;
        }
    }

    pub fn report_update_success(&self) {
        info!("Update succeeded - marking health as healthy");
        self.set_health_status(true);
    }

    pub fn report_update_failure(&self) {
        info!("Update failed - marking health as unhealthy");
        self.set_health_status(false);
    }
}
