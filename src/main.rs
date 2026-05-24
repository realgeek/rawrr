use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

mod config;
mod docker;
mod notifier;
mod rate_limiter;
mod state;

use config::Config;
use docker::DockerClient;
use notifier::{Notification, NotificationAction, Notifier};
use rate_limiter::RateLimiter;
use state::RawrrState;

pub struct Rawrr {
    config: Config,
    docker_client: DockerClient,
    state: RawrrState,
    rate_limiter: RateLimiter,
}

impl Rawrr {
    pub async fn new(config: Config) -> Result<Self> {
        let docker_client = DockerClient::new(&config.docker_host)?;
        let state = RawrrState::load(&config.state_file)?;
        
        let rate_limiter = RateLimiter::new(
            config.rate_limit_max_polls,
            config.rate_limit_window_secs,
        );
        
        Ok(Rawrr {
            config,
            docker_client,
            state,
            rate_limiter,
        })
    }
    
    pub async fn run(&mut self) -> Result<()> {
        info!("Rawrr starting (meow 🐾)");

        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;

        info!(
            "Waiting {} seconds before first poll...",
            self.config.startup_delay_secs
        );

        tokio::select! {
            _ = sleep(Duration::from_secs(self.config.startup_delay_secs)) => {}
            _ = sigterm.recv() => { info!("Received SIGTERM, shutting down"); return Ok(()); }
            _ = sigint.recv() =>  { info!("Received SIGINT, shutting down");  return Ok(()); }
        }

        loop {
            self.poll_and_upgrade().await;

            tokio::select! {
                _ = sleep(Duration::from_secs(self.config.poll_interval_secs)) => {}
                _ = sigterm.recv() => { info!("Received SIGTERM, shutting down"); break; }
                _ = sigint.recv() =>  { info!("Received SIGINT, shutting down");  break; }
            }
        }

        Ok(())
    }
    
    async fn poll_and_upgrade(&mut self) {
        if !self.rate_limiter.can_poll() {
            let wait_secs = self.rate_limiter.seconds_until_next_poll();
            warn!(
                "Rate limit exceeded, waiting {} seconds before next poll",
                wait_secs
            );
            return;
        }
        
        debug!("Starting poll cycle");
        
        // In a real implementation, you would:
        // 1. List all running containers from Docker
        // 2. Check their labels
        // 3. Query the registry for latest image digests
        // 4. Compare with state
        // 5. Handle upgrades based on labels
        
        // For now, this is a skeleton that shows the flow
        let containers = match self.docker_client.list_containers().await {
            Ok(containers) => containers,
            Err(e) => {
                error!("Failed to list containers: {}", e);
                return;
            }
        };
        
        self.rate_limiter.record_poll();
        self.state.last_poll_time = Utc::now();
        
        // Process each container
        for container in containers {
            if self.should_ignore(&container.labels) {
                debug!("Ignoring container: {}", container.id);
                continue;
            }
            
            let service_name = container
                .names
                .first()
                .unwrap_or(&container.id)
                .trim_start_matches('/')
                .to_string();
            
            match self.docker_client.get_image_digest(&container.image).await {
                Ok(digest) => {
                    debug!(
                        "Container {} has image digest: {}",
                        service_name, digest
                    );
                    
                    let old_digest = self.state.services
                        .get(&service_name)
                        .and_then(|s| s.image.as_ref())
                        .map(|img| img.digest.clone())
                        .unwrap_or_default();
                    
                    let digest_changed = old_digest != digest;
                    
                    // Update state with new digest
                    self.state.update_service_image(service_name.clone(), digest.clone());
                    
                    // Check if we should upgrade
                    if self.state.should_upgrade(
                        &service_name,
                        &digest,
                        self.config.get_release_delay(),
                    ) {
                        self.handle_upgrade(&service_name, &old_digest, &digest, &container.labels)
                            .await;
                    }
                }
                Err(e) => {
                    error!("Failed to get digest for {}: {}", service_name, e);
                }
            }
        }
        
        // Save state after poll
        if let Err(e) = self.state.save(&self.config.state_file) {
            error!("Failed to save state: {}", e);
        }
    }
    
    async fn handle_upgrade(
        &mut self,
        service_name: &str,
        old_digest: &str,
        new_digest: &str,
        labels: &HashMap<String, String>,
    ) {
        let notification = Notification {
            service_name: service_name.to_string(),
            old_digest: old_digest.to_string(),
            new_digest: new_digest.to_string(),
            action: if self.should_update(labels) {
                NotificationAction::Update
            } else {
                NotificationAction::NotifyOnly
            },
        };
        
        // Send notification
        if let Err(e) = Notifier::send(&self.config.notifier, &notification).await {
            error!("Failed to send notification: {}", e);
        }
        
        // Perform upgrade if needed
        if notification.action == NotificationAction::Update {
            info!("Upgrading container: {}", service_name);
            // TODO: Implement actual container upgrade
            // This would involve:
            // - Pulling the new image
            // - Stopping the current container
            // - Starting a new one with the same config
            // - Or using docker-compose if available
        }
    }
    
    fn should_ignore(&self, labels: &HashMap<String, String>) -> bool {
        labels
            .get(&self.config.label_ignore)
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false)
    }
    
    fn should_update(&self, labels: &HashMap<String, String>) -> bool {
        labels
            .get(&self.config.label_update)
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("rawrr=debug".parse()?),
        )
        .init();
    
    // Load config
    let config = Config::from_env()?;
    
    // Create and run Rawrr
    let mut rawrr = Rawrr::new(config).await?;
    rawrr.run().await?;
    
    Ok(())
}
