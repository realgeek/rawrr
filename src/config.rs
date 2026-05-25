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
    
    /// Container label that controls monitoring policy
    pub label_policy: String,
    
    /// Notification settings
    pub notifier: NotifierConfig,
    
    /// Rate limiting
    pub rate_limit_check_interval_secs: u64,
    pub rate_limit_max_polls: u32,
    pub rate_limit_window_secs: u64,

    /// Per-registry credentials: registry hostname -> (username, password/token)
    pub registry_credentials: HashMap<String, (String, String)>,

    /// When true, list containers and log their resolved policy, then stop without
    /// hitting any registry. Useful for verifying label changes.
    pub dry_run: bool,
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
        
        let label_policy = env::var("RAWRR_LABEL_POLICY")
            .unwrap_or_else(|_| "rawrr.policy".to_string());
        
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

        let dry_run = matches!(
            env::var("RAWRR_DRY_RUN").as_deref(),
            Ok("true") | Ok("1") | Ok("yes")
        );

        Ok(Config {
            state_file,
            startup_delay_secs,
            poll_interval_secs,
            release_delay_hours,
            docker_host,
            label_policy,
            notifier,
            rate_limit_check_interval_secs,
            rate_limit_max_polls,
            rate_limit_window_secs,
            registry_credentials,
            dry_run,
        })
    }
    
    pub fn get_release_delay(&self) -> Duration {
        Duration::hours(self.release_delay_hours)
    }
}

// Format: "docker.io=user:token,ghcr.io=user:ghp_token"
// Splits on the first '=' and first ':' so passwords containing ':' are handled.
// Commas and '=' are not supported in credentials themselves.
pub(crate) fn parse_registry_credentials(s: &str) -> HashMap<String, (String, String)> {
    s.split(',')
        .filter(|e| !e.is_empty())
        .filter_map(|entry| {
            let (registry, creds) = entry.trim().split_once('=')?;
            let (user, pass) = creds.split_once(':')?;
            Some((registry.to_string(), (user.to_string(), pass.to_string())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_credentials_empty() {
        assert!(parse_registry_credentials("").is_empty());
    }

    #[test]
    fn test_parse_credentials_single() {
        let creds = parse_registry_credentials("docker.io=user:token");
        assert_eq!(creds.get("docker.io"), Some(&("user".to_string(), "token".to_string())));
    }

    #[test]
    fn test_parse_credentials_multiple() {
        let creds = parse_registry_credentials("docker.io=user:tok1,ghcr.io=alice:ghp_abc");
        assert_eq!(creds.get("docker.io"), Some(&("user".to_string(), "tok1".to_string())));
        assert_eq!(creds.get("ghcr.io"), Some(&("alice".to_string(), "ghp_abc".to_string())));
        assert_eq!(creds.len(), 2);
    }

    #[test]
    fn test_parse_credentials_password_with_colon() {
        // Only the first ':' is the username/password separator
        let creds = parse_registry_credentials("docker.io=user:pass:word");
        assert_eq!(creds.get("docker.io"), Some(&("user".to_string(), "pass:word".to_string())));
    }

    #[test]
    fn test_parse_credentials_missing_equals() {
        assert!(parse_registry_credentials("docker.io").is_empty());
    }

    #[test]
    fn test_parse_credentials_missing_colon() {
        assert!(parse_registry_credentials("docker.io=useronly").is_empty());
    }

    #[test]
    fn test_get_release_delay() {
        let config = Config {
            state_file: "/tmp/state.json".into(),
            startup_delay_secs: 0,
            poll_interval_secs: 3600,
            release_delay_hours: 6,
            docker_host: String::new(),
            label_policy: String::new(),
            notifier: NotifierConfig::None,
            rate_limit_check_interval_secs: 60,
            rate_limit_max_polls: 100,
            rate_limit_window_secs: 3600,
            registry_credentials: Default::default(),
            dry_run: false,
        };
        assert_eq!(config.get_release_delay(), Duration::hours(6));
    }
}
