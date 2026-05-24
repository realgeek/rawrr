use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub names: Vec<String>,
    pub image: String,
    pub image_id: String,
    pub labels: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub digest: String,
    pub created: chrono::DateTime<chrono::Utc>,
}

pub struct DockerClient {
    client: reqwest::Client,
    socket_path: String,
}

impl DockerClient {
    pub fn new(docker_host: &str) -> Result<Self> {
        // For unix sockets, we'll use the socket_path from DOCKER_HOST
        // Format: unix:///var/run/docker.sock
        let socket_path = if docker_host.starts_with("unix://") {
            docker_host.strip_prefix("unix://").unwrap_or("/var/run/docker.sock").to_string()
        } else {
            docker_host.to_string()
        };
        
        let client = reqwest::Client::new();
        
        Ok(DockerClient {
            client,
            socket_path,
        })
    }
    
    pub async fn list_containers(&self) -> Result<Vec<Container>> {
        // This is a simplified version - in production you'd use docker-rs or similar
        // For now, we'll use the Docker API via HTTP over Unix socket
        // This requires special handling that's beyond a simple HTTP client
        
        // As a fallback for demonstration, we'll return an empty list
        // In real usage, integrate with docker-rs crate
        debug!("Listing containers from socket: {}", self.socket_path);
        
        // TODO: Implement proper Unix socket communication
        // For now, this is a placeholder
        Ok(vec![])
    }
    
    pub async fn inspect_container(&self, container_id: &str) -> Result<Container> {
        // TODO: Implement inspection
        Err(anyhow!("Not yet implemented"))
    }
    
    pub async fn get_image_digest(&self, image_ref: &str) -> Result<String> {
        // Query registry for image digest
        // This would hit Docker Hub, ECR, GitHub Container Registry, etc.
        debug!("Getting image digest for: {}", image_ref);
        
        // Parse the image reference
        let (registry, repository, tag) = parse_image_ref(image_ref)?;
        
        match registry.as_str() {
            "docker.io" | "" => get_docker_hub_digest(&repository, &tag).await,
            "ghcr.io" => get_github_digest(&repository, &tag).await,
            _ => get_generic_digest(&registry, &repository, &tag).await,
        }
    }
}

fn parse_image_ref(image: &str) -> Result<(String, String, String)> {
    let (name, tag) = if let Some(at) = image.rfind(':') {
        let (n, t) = image.split_at(at);
        (n, t.trim_start_matches(':').to_string())
    } else {
        (image, "latest".to_string())
    };
    
    let (registry, repo) = if let Some(slash) = name.find('/') {
        let (reg, r) = name.split_at(slash);
        if reg.contains('.') || reg.contains(':') {
            // It's a registry
            (reg.to_string(), r.trim_start_matches('/').to_string())
        } else {
            // It's a namespace on Docker Hub
            ("docker.io".to_string(), name.to_string())
        }
    } else {
        // Just a name on Docker Hub
        ("docker.io".to_string(), format!("library/{}", name))
    };
    
    Ok((registry, repo, tag))
}

async fn get_docker_hub_digest(repository: &str, tag: &str) -> Result<String> {
    // Docker Hub V2 API
    let url = format!("https://registry.hub.docker.com/v2/{}/manifests/{}", repository, tag);
    
    let client = reqwest::Client::new();
    let response = client
        .head(&url)
        .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .send()
        .await?;
    
    if !response.status().is_success() {
        return Err(anyhow!("Failed to get Docker Hub digest: {}", response.status()));
    }
    
    response
        .headers()
        .get("Docker-Content-Digest")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No Docker-Content-Digest header found"))
}

async fn get_github_digest(repository: &str, tag: &str) -> Result<String> {
    // GitHub Container Registry
    let url = format!("https://ghcr.io/v2/{}/manifests/{}", repository, tag);
    
    let client = reqwest::Client::new();
    let response = client
        .head(&url)
        .header("Accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await?;
    
    if !response.status().is_success() {
        return Err(anyhow!("Failed to get GHCR digest: {}", response.status()));
    }
    
    response
        .headers()
        .get("Docker-Content-Digest")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No Docker-Content-Digest header found"))
}

async fn get_generic_digest(registry: &str, repository: &str, tag: &str) -> Result<String> {
    // Generic OCI registry
    let url = format!("https://{}/v2/{}/manifests/{}", registry, repository, tag);
    
    let client = reqwest::Client::new();
    let response = client
        .head(&url)
        .header("Accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await?;
    
    if !response.status().is_success() {
        return Err(anyhow!("Failed to get digest from {}: {}", registry, response.status()));
    }
    
    response
        .headers()
        .get("Docker-Content-Digest")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No Docker-Content-Digest header found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_docker_hub_default() {
        let (reg, repo, tag) = parse_image_ref("nginx").unwrap();
        assert_eq!(reg, "docker.io");
        assert_eq!(repo, "library/nginx");
        assert_eq!(tag, "latest");
    }
    
    #[test]
    fn test_parse_docker_hub_with_tag() {
        let (reg, repo, tag) = parse_image_ref("nginx:1.25").unwrap();
        assert_eq!(reg, "docker.io");
        assert_eq!(repo, "library/nginx");
        assert_eq!(tag, "1.25");
    }
    
    #[test]
    fn test_parse_docker_hub_with_namespace() {
        let (reg, repo, tag) = parse_image_ref("myorg/myapp:v1.2.3").unwrap();
        assert_eq!(reg, "docker.io");
        assert_eq!(repo, "myorg/myapp");
        assert_eq!(tag, "v1.2.3");
    }
    
    #[test]
    fn test_parse_ghcr() {
        let (reg, repo, tag) = parse_image_ref("ghcr.io/myorg/myapp:latest").unwrap();
        assert_eq!(reg, "ghcr.io");
        assert_eq!(repo, "myorg/myapp");
        assert_eq!(tag, "latest");
    }
}
