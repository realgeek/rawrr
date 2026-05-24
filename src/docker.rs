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
    client: reqwest::Client,
}

impl DockerClient {
    pub fn new(docker_host: &str) -> Result<Self> {
        let docker = if docker_host.starts_with("unix://") {
            let path = docker_host.strip_prefix("unix://").unwrap_or("/var/run/docker.sock");
            Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)?
        } else {
            Docker::connect_with_http(docker_host, 120, bollard::API_DEFAULT_VERSION)?
        };
        Ok(DockerClient {
            docker,
            client: reqwest::Client::new(),
        })
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

        let url = match registry.as_str() {
            "docker.io" | "" => {
                format!("https://registry-1.docker.io/v2/{}/manifests/{}", repository, tag)
            }
            _ => format!("https://{}/v2/{}/manifests/{}", registry, repository, tag),
        };

        get_manifest_digest(&self.client, &url).await
    }
}

// Accepts all OCI and Docker manifest types so multi-platform image indexes
// (application/vnd.oci.image.index.v1+json) are returned alongside single-arch
// manifests. Registries like lscr.io return 404 if you request only the
// single-arch type for an image that is stored as an index.
const ACCEPT_MANIFESTS: &str = concat!(
    "application/vnd.oci.image.index.v1+json,",
    "application/vnd.oci.image.manifest.v1+json,",
    "application/vnd.docker.distribution.manifest.list.v2+json,",
    "application/vnd.docker.distribution.manifest.v2+json"
);

// Fetches a manifest digest, handling the standard OCI Bearer token challenge.
// All OCI-compliant registries (Docker Hub, GHCR, lscr.io, etc.) return 401 on
// the first request with a WWW-Authenticate header pointing to their token endpoint.
async fn get_manifest_digest(client: &reqwest::Client, url: &str) -> Result<String> {
    let response = client.head(url).header("Accept", ACCEPT_MANIFESTS).send().await?;

    let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let www_auth = response
            .headers()
            .get("WWW-Authenticate")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("401 with no WWW-Authenticate header"))?;

        let (realm, service, scope) = parse_www_authenticate(&www_auth)
            .ok_or_else(|| anyhow!("Could not parse WWW-Authenticate: {}", www_auth))?;

        let token = fetch_bearer_token(client, &realm, &service, &scope).await?;

        client
            .head(url)
            .header("Accept", ACCEPT_MANIFESTS)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?
    } else {
        response
    };

    if !response.status().is_success() {
        return Err(anyhow!("Registry request failed: {}", response.status()));
    }

    response
        .headers()
        .get("Docker-Content-Digest")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No Docker-Content-Digest header in response"))
}

async fn fetch_bearer_token(
    client: &reqwest::Client,
    realm: &str,
    service: &str,
    scope: &str,
) -> Result<String> {
    let mut url = reqwest::Url::parse(realm)?;
    {
        let mut q = url.query_pairs_mut();
        if !service.is_empty() {
            q.append_pair("service", service);
        }
        if !scope.is_empty() {
            q.append_pair("scope", scope);
        }
    }

    let body: serde_json::Value = client.get(url).send().await?.json().await?;

    body.get("token")
        .or_else(|| body.get("access_token"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No token field in auth response"))
}

fn parse_www_authenticate(header: &str) -> Option<(String, String, String)> {
    let params = header.strip_prefix("Bearer ")?;
    let realm = extract_quoted_param(params, "realm")?;
    let service = extract_quoted_param(params, "service").unwrap_or_default();
    let scope = extract_quoted_param(params, "scope").unwrap_or_default();
    Some((realm, service, scope))
}

fn extract_quoted_param(params: &str, key: &str) -> Option<String> {
    let needle = format!("{}=\"", key);
    let start = params.find(&needle)? + needle.len();
    let rest = &params[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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

    #[test]
    fn test_parse_www_authenticate() {
        let header = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull""#;
        let (realm, service, scope) = parse_www_authenticate(header).unwrap();
        assert_eq!(realm, "https://auth.docker.io/token");
        assert_eq!(service, "registry.docker.io");
        assert_eq!(scope, "repository:library/nginx:pull");
    }
}
