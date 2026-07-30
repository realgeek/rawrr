use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageState {
    /// The image digest/sha256
    pub digest: String,
    /// When we first saw this image
    pub first_seen: DateTime<Utc>,
    /// Last time we checked the registry
    pub last_checked: DateTime<Utc>,
    /// Last time we sent a reminder notification for this digest (Notify policy only)
    #[serde(default)]
    pub last_notified: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceState {
    /// Container name
    pub name: String,
    /// Current tracking image state
    pub image: Option<ImageState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawrrState {
    /// Last time we did a full poll (for rate limiting)
    pub last_poll_time: DateTime<Utc>,
    /// Per-service tracking
    pub services: HashMap<String, ServiceState>,
}

impl RawrrState {
    pub fn new() -> Self {
        RawrrState {
            last_poll_time: DateTime::UNIX_EPOCH,
            services: HashMap::new(),
        }
    }
    
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            let state = serde_json::from_str(&content)?;
            debug!("Loaded state from {:?}", path);
            Ok(state)
        } else {
            debug!("State file not found, creating new state");
            Ok(RawrrState::new())
        }
    }
    
    pub fn save(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let json = serde_json::to_string_pretty(&self)?;
        fs::write(path, json)?;
        debug!("Saved state to {:?}", path);
        Ok(())
    }
    
    pub fn get_or_create_service(&mut self, name: String) -> &mut ServiceState {
        self.services
            .entry(name.clone())
            .or_insert_with(|| ServiceState {
                name,
                image: None,
            })
    }
    
    pub fn update_service_image(
        &mut self,
        service_name: String,
        digest: String,
    ) {
        let service = self.get_or_create_service(service_name);
        let now = Utc::now();

        // If this is a new image, set first_seen to now and clear last_notified
        // (it's a different update we haven't reminded about yet).
        // If it's the same image, keep the original first_seen and last_notified.
        let same_digest = service
            .image
            .as_ref()
            .map(|img| img.digest == digest)
            .unwrap_or(false);

        let (first_seen, last_notified) = if same_digest {
            let img = service.image.as_ref().unwrap();
            (img.first_seen, img.last_notified)
        } else {
            (now, None)
        };

        service.image = Some(ImageState {
            digest,
            first_seen,
            last_checked: now,
            last_notified,
        });
    }
    
    /// Called after a container is successfully recreated. Resets first_seen to now
    /// so the release-delay window restarts and should_upgrade returns false until
    /// the next new digest appears.
    pub fn mark_upgraded(&mut self, service_name: &str) {
        if let Some(service) = self.services.get_mut(service_name) {
            if let Some(image) = &mut service.image {
                image.first_seen = Utc::now();
            }
        }
    }

    /// Called after a reminder notification is sent for a Notify-policy container.
    /// Records last_notified without touching first_seen, so the release-delay
    /// gate below stays anchored to the original release time.
    pub fn mark_notified(&mut self, service_name: &str) {
        if let Some(service) = self.services.get_mut(service_name) {
            if let Some(image) = &mut service.image {
                image.last_notified = Some(Utc::now());
            }
        }
    }

    /// True once release_delay has elapsed since first_seen, and either we've
    /// never notified about this digest or renotify_interval has elapsed since
    /// the last reminder.
    pub fn should_notify(
        &self,
        service_name: &str,
        current_digest: &str,
        release_delay: chrono::Duration,
        renotify_interval: chrono::Duration,
    ) -> bool {
        match self.services.get(service_name) {
            None => false,
            Some(service) => match &service.image {
                None => false,
                Some(image) => {
                    if image.digest != current_digest {
                        return false;
                    }
                    if Utc::now() - image.first_seen < release_delay {
                        return false;
                    }
                    match image.last_notified {
                        None => true,
                        Some(t) => Utc::now() - t >= renotify_interval,
                    }
                }
            },
        }
    }

    pub fn should_upgrade(
        &self,
        service_name: &str,
        current_digest: &str,
        release_delay: chrono::Duration,
    ) -> bool {
        match self.services.get(service_name) {
            None => false,
            Some(service) => match &service.image {
                None => false,
                Some(image) => {
                    // Upgrade if the digest matches and the delay has passed
                    if image.digest == current_digest {
                        let elapsed = Utc::now() - image.first_seen;
                        elapsed >= release_delay
                    } else {
                        false
                    }
                }
            },
        }
    }
}

impl Default for RawrrState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    #[test]
    fn test_new_state_is_empty() {
        let state = RawrrState::new();
        assert!(state.services.is_empty());
        assert_eq!(state.last_poll_time, DateTime::UNIX_EPOCH);
    }

    #[test]
    fn test_update_new_image_sets_first_seen() {
        let mut state = RawrrState::new();
        let before = Utc::now();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        let after = Utc::now();

        let img = state.services.get("svc").unwrap().image.as_ref().unwrap();
        assert_eq!(img.digest, "sha256:abc");
        assert!(img.first_seen >= before && img.first_seen <= after);
    }

    #[test]
    fn test_update_same_image_preserves_first_seen() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());

        let past = DateTime::UNIX_EPOCH;
        state.services.get_mut("svc").unwrap().image.as_mut().unwrap().first_seen = past;

        state.update_service_image("svc".to_string(), "sha256:abc".to_string());

        let img = state.services.get("svc").unwrap().image.as_ref().unwrap();
        assert_eq!(img.first_seen, past);
    }

    #[test]
    fn test_update_different_image_resets_first_seen() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:aaa".to_string());

        let past = DateTime::UNIX_EPOCH;
        state.services.get_mut("svc").unwrap().image.as_mut().unwrap().first_seen = past;

        state.update_service_image("svc".to_string(), "sha256:bbb".to_string());

        let img = state.services.get("svc").unwrap().image.as_ref().unwrap();
        assert_ne!(img.first_seen, past);
        assert_eq!(img.digest, "sha256:bbb");
    }

    #[test]
    fn test_should_upgrade_unknown_service() {
        let state = RawrrState::new();
        assert!(!state.should_upgrade("no_such", "sha256:abc", chrono::Duration::zero()));
    }

    #[test]
    fn test_should_upgrade_no_image() {
        let mut state = RawrrState::new();
        state.get_or_create_service("svc".to_string());
        assert!(!state.should_upgrade("svc", "sha256:abc", chrono::Duration::zero()));
    }

    #[test]
    fn test_should_upgrade_wrong_digest() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:aaa".to_string());
        assert!(!state.should_upgrade("svc", "sha256:different", chrono::Duration::zero()));
    }

    #[test]
    fn test_should_upgrade_delay_not_elapsed() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        // first_seen = now, so a 1-hour delay has not elapsed
        assert!(!state.should_upgrade("svc", "sha256:abc", chrono::Duration::hours(1)));
    }

    #[test]
    fn test_should_upgrade_delay_elapsed() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        // first_seen to UNIX epoch so any positive delay has long elapsed
        state.services.get_mut("svc").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;
        assert!(state.should_upgrade("svc", "sha256:abc", chrono::Duration::hours(6)));
    }

    #[test]
    fn test_mark_upgraded_resets_first_seen() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        state.services.get_mut("svc").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;

        let before = Utc::now();
        state.mark_upgraded("svc");
        let after = Utc::now();

        let first_seen = state.services.get("svc").unwrap().image.as_ref().unwrap().first_seen;
        assert!(first_seen >= before && first_seen <= after);
    }

    #[test]
    fn test_mark_upgraded_nonexistent_is_noop() {
        let mut state = RawrrState::new();
        state.mark_upgraded("no_such_service"); // must not panic
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = RawrrState::new();
        state.update_service_image("myapp".to_string(), "sha256:deadbeef".to_string());
        state.save(&path).unwrap();

        let loaded = RawrrState::load(&path).unwrap();
        let img = loaded.services.get("myapp").unwrap().image.as_ref().unwrap();
        assert_eq!(img.digest, "sha256:deadbeef");
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let state = RawrrState::load(&path).unwrap();
        assert!(state.services.is_empty());
    }

    // mark_upgraded is called after a real container recreation (Update policy);
    // it resets first_seen so should_upgrade doesn't immediately re-trigger.
    #[test]
    fn test_mark_upgraded_suppresses_reupgrade() {
        let mut state = RawrrState::new();
        state.update_service_image("plex".to_string(), "sha256:abc123".to_string());
        state.services.get_mut("plex").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;

        assert!(
            state.should_upgrade("plex", "sha256:abc123", chrono::Duration::hours(6)),
            "precondition: should_upgrade must be true before upgrading"
        );

        state.mark_upgraded("plex");

        assert!(
            !state.should_upgrade("plex", "sha256:abc123", chrono::Duration::hours(6)),
            "should not re-upgrade on the next poll after mark_upgraded resets first_seen"
        );
    }

    // Regression: an early-return path that skips saving state after mark_upgraded
    // would lose the reset on process restart and trigger a duplicate action.
    #[test]
    fn test_mark_upgraded_persists_across_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = RawrrState::new();
        state.update_service_image("plex".to_string(), "sha256:abc123".to_string());
        state.services.get_mut("plex").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;

        state.mark_upgraded("plex");
        state.save(&path).unwrap();

        let reloaded = RawrrState::load(&path).unwrap();
        assert!(
            !reloaded.should_upgrade("plex", "sha256:abc123", chrono::Duration::hours(6)),
            "should not re-notify after restart when state was saved post-notification"
        );
    }

    // --- should_notify / mark_notified ---

    #[test]
    fn test_should_notify_unknown_service() {
        let state = RawrrState::new();
        assert!(!state.should_notify(
            "no_such",
            "sha256:abc",
            chrono::Duration::zero(),
            chrono::Duration::zero()
        ));
    }

    #[test]
    fn test_should_notify_wrong_digest() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:aaa".to_string());
        assert!(!state.should_notify(
            "svc",
            "sha256:different",
            chrono::Duration::zero(),
            chrono::Duration::zero()
        ));
    }

    #[test]
    fn test_should_notify_release_delay_not_elapsed() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        // first_seen = now, so a 1-hour release delay has not elapsed
        assert!(!state.should_notify(
            "svc",
            "sha256:abc",
            chrono::Duration::hours(1),
            chrono::Duration::zero()
        ));
    }

    #[test]
    fn test_should_notify_true_before_first_notification() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        state.services.get_mut("svc").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;

        assert!(state.should_notify(
            "svc",
            "sha256:abc",
            chrono::Duration::hours(6),
            chrono::Duration::hours(24)
        ));
    }

    // Regression: renotify cadence must be governed by last_notified /
    // renotify_interval, not by resetting first_seen (which would also gate
    // should_upgrade and conflate the two policies' timers).
    #[test]
    fn test_mark_notified_suppresses_renotify_until_interval_elapses() {
        let mut state = RawrrState::new();
        state.update_service_image("plex".to_string(), "sha256:abc123".to_string());
        state.services.get_mut("plex").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;

        assert!(
            state.should_notify(
                "plex",
                "sha256:abc123",
                chrono::Duration::hours(6),
                chrono::Duration::hours(24)
            ),
            "precondition: should_notify must be true before the first notification"
        );

        state.mark_notified("plex");

        assert!(
            !state.should_notify(
                "plex",
                "sha256:abc123",
                chrono::Duration::hours(6),
                chrono::Duration::hours(24)
            ),
            "should not renotify immediately after mark_notified"
        );

        // first_seen must stay put — only last_notified governs the reminder cadence
        let img = state.services.get("plex").unwrap().image.as_ref().unwrap();
        assert_eq!(img.first_seen, DateTime::UNIX_EPOCH);

        // once the renotify interval has (notionally) elapsed, it should fire again
        state.services.get_mut("plex").unwrap().image.as_mut().unwrap().last_notified =
            Some(DateTime::UNIX_EPOCH);
        assert!(state.should_notify(
            "plex",
            "sha256:abc123",
            chrono::Duration::hours(6),
            chrono::Duration::hours(24)
        ));
    }

    #[test]
    fn test_mark_notified_persists_across_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = RawrrState::new();
        state.update_service_image("plex".to_string(), "sha256:abc123".to_string());
        state.services.get_mut("plex").unwrap().image.as_mut().unwrap().first_seen =
            DateTime::UNIX_EPOCH;

        state.mark_notified("plex");
        state.save(&path).unwrap();

        let reloaded = RawrrState::load(&path).unwrap();
        assert!(!reloaded.should_notify(
            "plex",
            "sha256:abc123",
            chrono::Duration::hours(6),
            chrono::Duration::hours(24)
        ));
    }

    #[test]
    fn test_update_service_image_same_digest_preserves_last_notified() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:abc".to_string());
        state.mark_notified("svc");
        let notified_at =
            state.services.get("svc").unwrap().image.as_ref().unwrap().last_notified;

        state.update_service_image("svc".to_string(), "sha256:abc".to_string());

        let img = state.services.get("svc").unwrap().image.as_ref().unwrap();
        assert_eq!(img.last_notified, notified_at);
    }

    #[test]
    fn test_update_service_image_new_digest_clears_last_notified() {
        let mut state = RawrrState::new();
        state.update_service_image("svc".to_string(), "sha256:aaa".to_string());
        state.mark_notified("svc");

        state.update_service_image("svc".to_string(), "sha256:bbb".to_string());

        let img = state.services.get("svc").unwrap().image.as_ref().unwrap();
        assert!(img.last_notified.is_none());
    }
}
