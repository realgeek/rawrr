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

#[derive(Debug, Clone, Copy, PartialEq)]
enum ContainerPolicy {
    Ignore,
    Notify,
    Update,
}

pub struct Rawrr {
    config: Config,
    docker_client: DockerClient,
    state: RawrrState,
    rate_limiter: RateLimiter,
}

impl Rawrr {
    pub async fn new(config: Config) -> Result<Self> {
        let docker_client = DockerClient::new(&config.docker_host, config.registry_credentials.clone())?;
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
        if self.config.dry_run {
            info!("Dry-run mode enabled — will log container policies and exit without hitting registries");
        }
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
        if !self.config.dry_run {
            let quiet_secs = self.config.poll_interval_secs.saturating_sub(5) as i64;
            let elapsed = (Utc::now() - self.state.last_poll_time).num_seconds();
            if elapsed < quiet_secs {
                info!(
                    "Last poll was {}s ago, quiet period is {}s — skipping",
                    elapsed, quiet_secs
                );
                return;
            }
        }

        if !self.rate_limiter.can_poll() {
            let wait_secs = self.rate_limiter.seconds_until_next_poll();
            warn!(
                "Rate limit exceeded, waiting {} seconds before next poll",
                wait_secs
            );
            return;
        }
        
        debug!("Starting poll cycle");

        let containers = match self.docker_client.list_containers().await {
            Ok(containers) => containers,
            Err(e) => {
                error!("Failed to list containers: {}", e);
                return;
            }
        };
        
        self.rate_limiter.record_poll();
        self.state.last_poll_time = Utc::now();

        if self.config.dry_run {
            info!("Dry-run: found {} container(s)", containers.len());
            for container in &containers {
                let service_name = container
                    .names
                    .first()
                    .unwrap_or(&container.id)
                    .trim_start_matches('/')
                    .to_string();
                let policy_label = container
                    .labels
                    .get(&self.config.label_policy)
                    .map(|v| v.as_str())
                    .unwrap_or("(none)");
                let resolved = match self.get_container_policy(&container.labels) {
                    Some(ContainerPolicy::Ignore) => "ignore",
                    Some(ContainerPolicy::Notify) => "notify",
                    Some(ContainerPolicy::Update) => "update",
                    None => "(skipped — no recognised policy)",
                };
                info!(
                    "Dry-run:  {:<30}  image={:<45}  label={:<10}  resolved={}",
                    service_name, container.image, policy_label, resolved
                );
            }
            return;
        }

        // Process each container
        for container in containers {
            let service_name = container
                .names
                .first()
                .unwrap_or(&container.id)
                .trim_start_matches('/')
                .to_string();

            let policy = match self.get_container_policy(&container.labels) {
                Some(ContainerPolicy::Ignore) => {
                    debug!("Ignoring container: {}", service_name);
                    continue;
                }
                None => {
                    debug!("No policy label on container {}, skipping", service_name);
                    continue;
                }
                Some(p) => p,
            };

            match self.docker_client.get_image_digest(&container.image).await {
                Ok(digest) => {
                    debug!("Container {} has image digest: {}", service_name, short_digest(&digest));

                    let old_digest = self.state.services
                        .get(&service_name)
                        .and_then(|s| s.image.as_ref())
                        .map(|img| img.digest.clone())
                        .unwrap_or_default();

                    self.state.update_service_image(service_name.clone(), digest.clone());

                    if self.state.should_upgrade(
                        &service_name,
                        &digest,
                        self.config.get_release_delay(),
                    ) {
                        let already_current = if !container.image_id.is_empty() {
                            match self.docker_client.get_local_image_digest(&container.image_id).await {
                                Ok(Some(ref local)) => local == &digest,
                                Ok(None) => false,
                                Err(e) => {
                                    warn!("Could not read local image digest for {}: {}", service_name, e);
                                    false
                                }
                            }
                        } else {
                            false
                        };

                        if already_current {
                            debug!("{} is already running {}, skipping upgrade", service_name, short_digest(&digest));
                        } else {
                            self.handle_upgrade(
                                &service_name,
                                &container.id,
                                &container.image,
                                &old_digest,
                                &digest,
                                policy,
                            ).await;
                        }
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
        container_id: &str,
        image: &str,
        old_digest: &str,
        new_digest: &str,
        policy: ContainerPolicy,
    ) {
        let notification = Notification {
            service_name: service_name.to_string(),
            old_digest: old_digest.to_string(),
            new_digest: new_digest.to_string(),
            action: if policy == ContainerPolicy::Update {
                NotificationAction::Update
            } else {
                NotificationAction::NotifyOnly
            },
        };

        if let Err(e) = Notifier::send(&self.config.notifier, &notification).await {
            error!("Failed to send notification for {}: {}", service_name, e);
        }

        if notification.action == NotificationAction::Update {
            info!("Upgrading {}", service_name);
            if let Err(e) = self.docker_client.pull_image(image).await {
                error!("Failed to pull image for {}: {}", service_name, e);
                return;
            }
            if let Err(e) = self.docker_client.recreate_container(container_id).await {
                error!("Failed to recreate container {}: {}", service_name, e);
            }
        }
    }
    
    fn get_container_policy(&self, labels: &HashMap<String, String>) -> Option<ContainerPolicy> {
        match labels.get(&self.config.label_policy)?.to_lowercase().as_str() {
            "ignore" | "false" | "off" => Some(ContainerPolicy::Ignore),
            "notify" => Some(ContainerPolicy::Notify),
            "update" => Some(ContainerPolicy::Update),
            other => {
                warn!("Unrecognised policy value {:?}, skipping container", other);
                None
            }
        }
    }
}

fn short_digest(digest: &str) -> &str {
    let hash_start = digest.find(':').map(|i| i + 1).unwrap_or(0);
    let end = (hash_start + 12).min(digest.len());
    &digest[..end]
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
