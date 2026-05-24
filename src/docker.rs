use anyhow::{anyhow, Result};
use bollard::container::ListContainersOptions;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub names: Vec<String>,
    pub image: String,
    pub image_id: String,
    pub labels: HashMap<String, String>,
}

pub struct DockerClient {
    docker: Docker,
}

impl DockerClient {
    pub fn new(docker_host: &str) -> Result<Self> {
        let docker = if docker_host.starts_with("unix://") {
            let path = docker_host.strip_prefix("unix://").unwrap_or("/var/run/docker.sock");
            Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)?
        } else {
            Docker::connect_with_http(docker_host, 120, bollard::API_DEFAULT_VERSION)?
        };
        Ok(DockerClient { docker })
    }

    pub async fn list_containers(&self) -> Result<Vec<Container>> {
        let summaries = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: false,
                ..Default::default()
            }))
            .await?;

        Ok(summaries
            .into_iter()
            .map(|c| Container {
                id: c.id.unwrap_or_default(),
                names: c.names.unwrap_or_default(),
                image: c.image.unwrap_or_default(),
                image_id: c.image_id.unwrap_or_default(),
                labels: c.labels.unwrap_or_default(),
            })
            .collect())
    }

    pub async fn inspect_container(&self, container_id: &str) -> Result<Container> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let config = info.config.unwrap_or_default();
        Ok(Container {
            id: info.id.unwrap_or_default(),
            names: info.name.map(|n| vec![n]).unwrap_or_default(),
            image: config.image.unwrap_or_default(),
            image_id: info.image.unwrap_or_default(),
            labels: config.labels.unwrap_or_default(),
        })
    }

    pub async fn get_image_digest(&self, image_ref: &str) -> Result<String> {
        debug!("Getting image digest for: {}", image_ref);
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
            (reg.to_string(), r.trim_start_matches('/').to_string())
        } else {
            ("docker.io".to_string(), name.to_string())
        }
    } else {
        ("docker.io".to_string(), format!("library/{}", name))
    };

    Ok((registry, repo, tag))
}

async fn get_docker_hub_digest(repository: &str, tag: &str) -> Result<String> {
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
