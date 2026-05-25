use anyhow::Result;
use chrono::Utc;
use std::collections::{HashMap, VecDeque};
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
use docker::{ContainerSpec, DockerClient};
use notifier::{Notification, NotificationAction, Notifier};
use rate_limiter::RateLimiter;
use state::RawrrState;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ContainerPolicy {
    Ignore,
    Notify,
    Update,
}

struct PendingUpgrade {
    service_name: String,
    container_id: String,
    image: String,
    old_digest: String,
    new_digest: String,
    policy: ContainerPolicy,
    compose_project: Option<String>,
    depends_on: Vec<String>,
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

        // Phase 1: Scan — check every relevant container and collect those due for an upgrade
        let mut pending: Vec<PendingUpgrade> = Vec::new();

        for container in &containers {
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

            let digest = match self.docker_client.get_image_digest(&container.image).await {
                Ok(d) => d,
                Err(e) => {
                    error!("Failed to get digest for {}: {}", service_name, e);
                    continue;
                }
            };

            debug!("Container {} has image digest: {}", service_name, short_digest(&digest));

            let old_digest = self.state.services
                .get(&service_name)
                .and_then(|s| s.image.as_ref())
                .map(|img| img.digest.clone())
                .unwrap_or_default();

            self.state.update_service_image(service_name.clone(), digest.clone());

            if !self.state.should_upgrade(&service_name, &digest, self.config.get_release_delay()) {
                continue;
            }

            if !container.image_id.is_empty() {
                match self.docker_client.get_local_image_digest(&container.image_id).await {
                    Ok(Some(ref local)) if local == &digest => {
                        debug!("{} already running {}, skipping", service_name, short_digest(&digest));
                        continue;
                    }
                    Err(e) => warn!("Could not read local image digest for {}: {}", service_name, e),
                    _ => {}
                }
            }

            pending.push(PendingUpgrade {
                service_name,
                container_id: container.id.clone(),
                image: container.image.clone(),
                old_digest,
                new_digest: digest,
                policy,
                compose_project: container.labels.get("com.docker.compose.project").cloned(),
                depends_on: parse_depends_on(&container.labels),
            });
        }

        if let Err(e) = self.state.save(&self.config.state_file) {
            error!("Failed to save state: {}", e);
        }

        if pending.is_empty() {
            return;
        }

        // Phase 2: Notify all pending upgrades
        for upgrade in &pending {
            let notification = Notification {
                service_name: upgrade.service_name.clone(),
                old_digest: upgrade.old_digest.clone(),
                new_digest: upgrade.new_digest.clone(),
                action: if upgrade.policy == ContainerPolicy::Update {
                    NotificationAction::Update
                } else {
                    NotificationAction::NotifyOnly
                },
            };
            if let Err(e) = Notifier::send(&self.config.notifier, &notification).await {
                error!("Failed to send notification for {}: {}", upgrade.service_name, e);
            }
        }

        // From here on, only Update-policy containers need action
        let update_indices: Vec<usize> = pending.iter()
            .enumerate()
            .filter(|(_, u)| u.policy == ContainerPolicy::Update)
            .map(|(i, _)| i)
            .collect();

        if update_indices.is_empty() {
            return;
        }

        // Inspect all containers before stopping any — preserves their config
        let mut specs: HashMap<String, ContainerSpec> = HashMap::new();
        for &idx in &update_indices {
            let u = &pending[idx];
            match self.docker_client.inspect_for_recreate(&u.container_id).await {
                Ok(spec) => { specs.insert(u.container_id.clone(), spec); }
                Err(e) => error!("Failed to inspect {}: {}", u.service_name, e),
            }
        }

        // Pull all new images while everything is still running
        for &idx in &update_indices {
            let u = &pending[idx];
            if let Err(e) = self.docker_client.pull_image(&u.image).await {
                error!("Failed to pull image for {}: {}", u.service_name, e);
            }
        }

        // Phase 3: Group by compose project, sort by dependency, restart in order
        let mut groups: HashMap<Option<String>, Vec<usize>> = HashMap::new();
        for &idx in &update_indices {
            groups.entry(pending[idx].compose_project.clone()).or_default().push(idx);
        }

        for (project, group_indices) in groups {
            let group: Vec<&PendingUpgrade> = group_indices.iter().map(|&i| &pending[i]).collect();
            let order = topological_sort(&group);

            if let Some(ref name) = project {
                info!("Upgrading compose project '{}' ({} service(s))", name, group.len());
            }

            // Stop in reverse dependency order: dependents first, dependencies last
            for &i in order.iter().rev() {
                let u = group[i];
                if let Err(e) = self.docker_client.stop_and_remove(&u.container_id, &u.service_name).await {
                    error!("Failed to stop {}: {}", u.service_name, e);
                }
            }

            // Start in dependency order: dependencies first, dependents last
            for &i in &order {
                let u = group[i];
                match specs.get(&u.container_id) {
                    Some(spec) => {
                        if let Err(e) = self.docker_client.create_and_start(spec).await {
                            error!("Failed to start {}: {}", u.service_name, e);
                        } else {
                            self.state.mark_upgraded(&u.service_name);
                        }
                    }
                    None => error!("No spec for {}, skipping start", u.service_name),
                }
            }
        }

        if let Err(e) = self.state.save(&self.config.state_file) {
            error!("Failed to save state after upgrades: {}", e);
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

/// Reads dependency info from container labels.
/// Prefers the compose-native label (format: `svc:condition:required,...`);
/// falls back to `rawrr.depends_on` (format: `svc1,svc2`) for non-compose containers.
fn parse_depends_on(labels: &HashMap<String, String>) -> Vec<String> {
    if let Some(v) = labels.get("com.docker.compose.depends_on") {
        return v
            .split(',')
            .filter_map(|entry| entry.split(':').next().map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Some(v) = labels.get("rawrr.depends_on") {
        return v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    vec![]
}

/// Kahn's algorithm topological sort. Returns indices into `upgrades` in
/// dependency-first order (dependencies before the services that need them).
/// On a cycle, appends the remaining nodes in arbitrary order with a warning.
fn topological_sort(upgrades: &[&PendingUpgrade]) -> Vec<usize> {
    let n = upgrades.len();
    let name_to_idx: HashMap<&str, usize> = upgrades
        .iter()
        .enumerate()
        .map(|(i, u)| (u.service_name.as_str(), i))
        .collect();

    // adj[dep] = list of dependent indices; in_degree[i] = number of unresolved deps
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_degree = vec![0usize; n];

    for (i, upgrade) in upgrades.iter().enumerate() {
        for dep_name in &upgrade.depends_on {
            if let Some(&dep_idx) = name_to_idx.get(dep_name.as_str()) {
                adj[dep_idx].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    while let Some(idx) = queue.pop_front() {
        order.push(idx);
        for &dependent in &adj[idx] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }

    if order.len() < n {
        warn!("Dependency cycle detected; appending remaining containers in arbitrary order");
        for i in 0..n {
            if !order.contains(&i) {
                order.push(i);
            }
        }
    }

    order
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
