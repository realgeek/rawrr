use anyhow::Result;
use chrono::Duration;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Path to state file (tracks last seen images and poll times)
    pub state_file: PathBuf,
    
    /// Startup delay in seconds before first poll
    pub startup_delay_secs: u64,
    
    /// Poll interval in seconds
    pub poll_interval_secs: u64,
    
    /// Delay between image release and upgrade (in hours)
    pub release_delay_hours: i64,
    
    /// Docker socket or remote endpoint
    pub docker_host: String,
    
    /// Container labels that determine behavior
    pub label_ignore: String,
    pub label_notify: String,
    pub label_update: String,
    
    /// Notification settings
    pub notifier: NotifierConfig,
    
    /// Rate limiting
    pub rate_limit_check_interval_secs: u64,
    pub rate_limit_max_polls: u32,
    pub rate_limit_window_secs: u64,

    /// Per-registry credentials: registry hostname -> (username, password/token)
    pub registry_credentials: HashMap<String, (String, String)>,
}

#[derive(Debug, Clone)]
pub enum NotifierConfig {
    Gotify {
        url: String,
        token: String,
    },
    Ntfy {
        url: String,
        topic: String,
    },
    None,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();
        
        let state_file = env::var("RAWRR_STATE_FILE")
            .unwrap_or_else(|_| "/var/lib/rawrr/state.json".to_string())
            .into();
        
        let startup_delay_secs = env::var("RAWRR_STARTUP_DELAY_SECS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()?;
        
        let poll_interval_secs = env::var("RAWRR_POLL_INTERVAL_SECS")
            .unwrap_or_else(|_| "3600".to_string()) // 1 hour default
            .parse()?;
        
        let release_delay_hours = env::var("RAWRR_RELEASE_DELAY_HOURS")
            .unwrap_or_else(|_| "6".to_string())
            .parse()?;
        
        let docker_host = env::var("DOCKER_HOST")
            .unwrap_or_else(|_| "unix:///var/run/docker.sock".to_string());
        
        let label_ignore = env::var("RAWRR_LABEL_IGNORE")
            .unwrap_or_else(|_| "rawrr.ignore".to_string());
        
        let label_notify = env::var("RAWRR_LABEL_NOTIFY")
            .unwrap_or_else(|_| "rawrr.notify".to_string());
        
        let label_update = env::var("RAWRR_LABEL_UPDATE")
            .unwrap_or_else(|_| "rawrr.update".to_string());
        
        let notifier = match env::var("RAWRR_NOTIFIER").as_deref() {
            Ok("gotify") => {
                let url = env::var("RAWRR_GOTIFY_URL")
                    .expect("RAWRR_GOTIFY_URL required when using Gotify");
                let token = env::var("RAWRR_GOTIFY_TOKEN")
                    .expect("RAWRR_GOTIFY_TOKEN required when using Gotify");
                NotifierConfig::Gotify { url, token }
            }
            Ok("ntfy") => {
                let url = env::var("RAWRR_NTFY_URL")
                    .unwrap_or_else(|_| "https://ntfy.sh".to_string());
                let topic = env::var("RAWRR_NTFY_TOPIC")
                    .expect("RAWRR_NTFY_TOPIC required when using ntfy");
                NotifierConfig::Ntfy { url, topic }
            }
            _ => NotifierConfig::None,
        };
        
        let rate_limit_check_interval_secs = env::var("RAWRR_RATE_LIMIT_CHECK_INTERVAL_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()?;
        
        let rate_limit_max_polls = env::var("RAWRR_RATE_LIMIT_MAX_POLLS")
            .unwrap_or_else(|_| "100".to_string())
            .parse()?;
        
        let rate_limit_window_secs = env::var("RAWRR_RATE_LIMIT_WINDOW_SECS")
            .unwrap_or_else(|_| "3600".to_string()) // 1 hour window
            .parse()?;

        let registry_credentials = parse_registry_credentials(
            &env::var("RAWRR_REGISTRY_CREDENTIALS").unwrap_or_default(),
        );

        Ok(Config {
            state_file,
            startup_delay_secs,
            poll_interval_secs,
            release_delay_hours,
            docker_host,
            label_ignore,
            label_notify,
            label_update,
            notifier,
            rate_limit_check_interval_secs,
            rate_limit_max_polls,
            rate_limit_window_secs,
            registry_credentials,
        })
    }
    
    pub fn get_release_delay(&self) -> Duration {
        Duration::hours(self.release_delay_hours)
    }
}

// Format: "docker.io=user:token,ghcr.io=user:ghp_token"
// Splits on the first '=' and first ':' so passwords containing ':' are handled.
// Commas and '=' are not supported in credentials themselves.
fn parse_registry_credentials(s: &str) -> HashMap<String, (String, String)> {
    s.split(',')
        .filter(|e| !e.is_empty())
        .filter_map(|entry| {
            let (registry, creds) = entry.trim().split_once('=')?;
            let (user, pass) = creds.split_once(':')?;
            Some((registry.to_string(), (user.to_string(), pass.to_string())))
        })
        .collect()
}
