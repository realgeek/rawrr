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
            last_poll_time: Utc::now(),
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
        
        // If this is a new image, set first_seen to now
        // If it's the same image, keep the original first_seen
        let first_seen = if service
            .image
            .as_ref()
            .map(|img| img.digest == digest)
            .unwrap_or(false)
        {
            service.image.as_ref().unwrap().first_seen
        } else {
            now
        };
        
        service.image = Some(ImageState {
            digest,
            first_seen,
            last_checked: now,
        });
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
