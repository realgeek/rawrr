use crate::config::NotifierConfig;
use anyhow::Result;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct Notification {
    pub service_name: String,
    pub old_digest: String,
    pub new_digest: String,
    pub action: NotificationAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationAction {
    Update,
    NotifyOnly,
}

pub struct Notifier;

impl Notifier {
    pub async fn send(config: &NotifierConfig, notification: &Notification) -> Result<()> {
        match config {
            NotifierConfig::Gotify { url, token } => {
                send_gotify(url, token, notification).await
            }
            NotifierConfig::Ntfy { url, topic } => {
                send_ntfy(url, topic, notification).await
            }
            NotifierConfig::None => {
                debug!("Notifier disabled, skipping notification");
                Ok(())
            }
        }
    }
}

async fn send_gotify(url: &str, token: &str, notification: &Notification) -> Result<()> {
    let title = match notification.action {
        NotificationAction::Update => {
            format!("🐳 {} - Update Available & Applied", notification.service_name)
        }
        NotificationAction::NotifyOnly => {
            format!("ℹ️ {} - Update Available", notification.service_name)
        }
    };
    
    let message = format!(
        "Old digest: {}\nNew digest: {}",
        truncate_digest(&notification.old_digest),
        truncate_digest(&notification.new_digest)
    );
    
    let payload = serde_json::json!({
        "title": title,
        "message": message,
        "priority": match notification.action {
            NotificationAction::Update => 5,
            NotificationAction::NotifyOnly => 3,
        }
    });
    
    let client = reqwest::Client::new();
    let url = format!("{}/message?token={}", url.trim_end_matches('/'), token);
    
    let response = client.post(&url).json(&payload).send().await?;
    
    if !response.status().is_success() {
        warn!("Failed to send Gotify notification: {}", response.status());
        return Err(anyhow::anyhow!("Gotify notification failed: {}", response.status()));
    }
    
    debug!("Sent Gotify notification for {}", notification.service_name);
    Ok(())
}

async fn send_ntfy(url: &str, topic: &str, notification: &Notification) -> Result<()> {
    let title = match notification.action {
        NotificationAction::Update => {
            format!("🐳 {} - Update Applied", notification.service_name)
        }
        NotificationAction::NotifyOnly => {
            format!("ℹ️ {} - Update Available", notification.service_name)
        }
    };
    
    let message = format!(
        "Old: {}\nNew: {}",
        truncate_digest(&notification.old_digest),
        truncate_digest(&notification.new_digest)
    );
    
    let client = reqwest::Client::new();
    let url = format!("{}/{}", url.trim_end_matches('/'), topic);
    
    let response = client
        .post(&url)
        .header("Title", &title)
        .body(message)
        .send()
        .await?;
    
    if !response.status().is_success() {
        warn!("Failed to send ntfy notification: {}", response.status());
        return Err(anyhow::anyhow!("ntfy notification failed: {}", response.status()));
    }
    
    debug!("Sent ntfy notification for {}", notification.service_name);
    Ok(())
}

fn truncate_digest(digest: &str) -> String {
    let hash_start = digest.find(':').map(|i| i + 1).unwrap_or(0);
    let end = (hash_start + 12).min(digest.len());
    if end < digest.len() {
        format!("{}…", &digest[..end])
    } else {
        digest.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_digest_sha256_prefix() {
        // "sha256:" = 7 chars, hash_start = 7, end = 7+12 = 19 < len → truncated
        let digest = "sha256:abcdefghijklmnopqrstuvwxyz";
        assert_eq!(truncate_digest(digest), "sha256:abcdefghijkl…");
    }

    #[test]
    fn test_truncate_digest_no_colon() {
        // No prefix: hash_start = 0, end = 12 < 26 → truncated
        let digest = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(truncate_digest(digest), "abcdefghijkl…");
    }

    #[test]
    fn test_truncate_digest_short_input() {
        // Short enough that end == len → returned unchanged
        let digest = "sha256:abc";
        assert_eq!(truncate_digest(digest), "sha256:abc");
    }

    #[test]
    fn test_truncate_digest_exactly_12_hash_chars() {
        // "sha256:" + exactly 12 hash chars = 19 chars total, end == len → no ellipsis
        let digest = "sha256:abcdefghijkl";
        assert_eq!(truncate_digest(digest), "sha256:abcdefghijkl");
    }
}
