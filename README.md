# Rawrr

A Rust-based Docker image update watcher that monitors your containers for new images and applies updates based on configurable policies. Think of it as a smarter, more flexible alternative to Diun or Watchtower with fine-grained control over when and how updates happen.

## Features

- **Configurable Release Delay**: Only upgrade images that have been released for at least X hours (default: 6 hours). This prevents chasing unstable releases.
- **Per-Container Update Policies**: Use Docker labels to control behavior per service:
  - `rawrr.ignore=true` - Skip this container entirely
  - `rawrr.notify=true` - Send notifications only, no auto-upgrade
  - `rawrr.update=true` - Automatically pull and restart on update
- **Smart Polling**: Respects registry API rate limits and tracks poll history to avoid throttling
- **Startup Delay**: Configurable delay before first poll (useful in orchestrated environments)
- **Flexible Notifications**: Support for both Gotify and ntfy with easy future expansion
- **State Tracking**: Remembers image digests and first-seen timestamps across restarts
- **Multi-Registry Support**: Works with Docker Hub, GitHub Container Registry, and generic OCI registries

## Why Rawrr?

- **Diun's limitations**: No per-image release age checking
- **Watchtower's limitations**: Limited control over update timing and policies
- **Rawrr's approach**: Combines the best of both worlds with:
  - Separate polling and decision phases
  - Registry-aware digest comparison
  - Per-service label-based policies
  - Proper rate limiting

## Installation

### Build from Source

```bash
git clone https://github.com/yourusername/rawrr.git
cd rawrr
cargo build --release
./target/release/rawrr
```

### Docker

```dockerfile
FROM rust:latest as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/rawrr /usr/local/bin/
ENTRYPOINT ["rawrr"]
```

## Configuration

All configuration is done via environment variables:

### Core Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `RAWRR_STARTUP_DELAY_SECS` | `30` | Seconds to wait before first poll |
| `RAWRR_POLL_INTERVAL_SECS` | `3600` | Poll interval in seconds (1 hour) |
| `RAWRR_RELEASE_DELAY_HOURS` | `6` | Hours to wait before upgrading after release |
| `RAWRR_STATE_FILE` | `/var/lib/rawrr/state.json` | Path to state persistence file |

### Docker Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `DOCKER_HOST` | `unix:///var/run/docker.sock` | Docker daemon socket or TCP endpoint |

### Label Names (Customize if needed)

| Variable | Default | Description |
|----------|---------|-------------|
| `RAWRR_LABEL_IGNORE` | `rawrr.ignore` | Label name for ignoring containers |
| `RAWRR_LABEL_NOTIFY` | `rawrr.notify` | Label name for notify-only mode |
| `RAWRR_LABEL_UPDATE` | `rawrr.update` | Label name for auto-update mode |

### Rate Limiting

| Variable | Default | Description |
|----------|---------|-------------|
| `RAWRR_RATE_LIMIT_MAX_POLLS` | `100` | Max registry queries per window |
| `RAWRR_RATE_LIMIT_WINDOW_SECS` | `3600` | Rate limit window duration |

### Notification Settings

#### Gotify

```bash
RAWRR_NOTIFIER=gotify
RAWRR_GOTIFY_URL=http://gotify.example.com
RAWRR_GOTIFY_TOKEN=your-token-here
```

#### ntfy

```bash
RAWRR_NOTIFIER=ntfy
RAWRR_NTFY_URL=https://ntfy.sh  # Optional, defaults to https://ntfy.sh
RAWRR_NTFY_TOPIC=my-docker-updates
```

#### Disabled

```bash
RAWRR_NOTIFIER=""  # Or just omit it
```

## Usage Examples

### Example 1: Basic Setup with Gotify

```yaml
version: '3.8'

services:
  rawrr:
    image: rawrr:latest
    environment:
      RAWRR_RELEASE_DELAY_HOURS: "6"
      RAWRR_POLL_INTERVAL_SECS: "3600"
      RAWRR_NOTIFIER: "gotify"
      RAWRR_GOTIFY_URL: "http://gotify:80"
      RAWRR_GOTIFY_TOKEN: "my-token"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - rawrr_state:/var/lib/rawrr

  nginx:
    image: nginx:latest
    labels:
      rawrr.update: "true"  # Auto-update after 6 hours
    ports:
      - "80:80"
```

### Example 2: Conservative Approach (Notify Only)

```yaml
postgres:
  image: postgres:15
  labels:
    rawrr.notify: "true"  # Only send notifications
  environment:
    POSTGRES_PASSWORD: "secret"
```

### Example 3: Aggressive Delay (24 hours)

```bash
RAWRR_RELEASE_DELAY_HOURS=24
RAWRR_POLL_INTERVAL_SECS=3600  # Still poll hourly
```

This polls hourly but only upgrades images released 24+ hours ago.

### Example 4: ntfy Notifications

```yaml
services:
  rawrr:
    image: rawrr:latest
    environment:
      RAWRR_NOTIFIER: "ntfy"
      RAWRR_NTFY_TOPIC: "docker-updates"  # Visit https://ntfy.sh/docker-updates
      RAWRR_RELEASE_DELAY_HOURS: "6"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - rawrr_state:/var/lib/rawrr
```

## How It Works

### The Two-Phase Approach

1. **Polling Phase** (every `RAWRR_POLL_INTERVAL_SECS`):
   - Query registries for latest image digests
   - Store first-seen timestamp for new images
   - Update state file

2. **Decision Phase** (same as polling):
   - Check if image first-seen time > `RAWRR_RELEASE_DELAY_HOURS`
   - If yes and container has `rawrr.update=true`, trigger upgrade
   - Send notifications based on container labels

### Example Timeline

```
10:00 - Poll: nginx:latest = sha256:abc123 (first seen)
        [Store first_seen = 10:00]
        
11:00 - Poll: nginx:latest = sha256:abc123 (same)
        [No upgrade yet, only 1 hour < 6 hours]

16:00 - Poll: nginx:latest = sha256:abc123 (same)
        [6 hours elapsed! Upgrade and restart if rawrr.update=true]
        [Send notification]

17:00 - Poll: nginx:latest = sha256:def456 (new release)
        [Store first_seen = 17:00]
        [Upgrade only if it's still the latest at 23:00]
```

## State File Format

Rawrr persists state as JSON for transparency and debugging:

```json
{
  "last_poll_time": "2024-05-22T16:30:45.123456Z",
  "services": {
    "nginx-web": {
      "name": "nginx-web",
      "image": {
        "digest": "sha256:abc123...",
        "first_seen": "2024-05-22T10:00:00.000000Z",
        "last_checked": "2024-05-22T16:30:45.123456Z"
      }
    },
    "postgres-db": {
      "name": "postgres-db",
      "image": {
        "digest": "sha256:def456...",
        "first_seen": "2024-05-22T12:00:00.000000Z",
        "last_checked": "2024-05-22T16:30:45.123456Z"
      }
    }
  }
}
```

## Rate Limiting

Rawrr includes built-in rate limit protection to avoid hitting registry API limits:

- Default: 100 registry queries per hour
- Each container check = 1 query
- If limit reached, polling is skipped with a warning

Adjust via:

```bash
RAWRR_RATE_LIMIT_MAX_POLLS=200       # More aggressive
RAWRR_RATE_LIMIT_WINDOW_SECS=3600    # Different window
```

## Troubleshooting

### "Rate limit exceeded"

Increase the interval or limit:
```bash
RAWRR_POLL_INTERVAL_SECS=7200  # Poll every 2 hours instead of 1
RAWRR_RATE_LIMIT_MAX_POLLS=200
```

### Notifications not sending

Check Gotify/ntfy connectivity:
```bash
# Test Gotify
curl -X POST http://gotify:80/message?token=TOKEN \
  -H "Content-Type: application/json" \
  -d '{"title":"Test","message":"Hello"}'

# Test ntfy
curl -d "Hello from Rawrr" https://ntfy.sh/my-topic
```

### State file not updating

Verify volume permissions:
```bash
ls -la /var/lib/rawrr/
```

## Future Enhancements

- [ ] Actual Docker container restart implementation
- [ ] Support for docker-compose orchestration
- [ ] Additional notifiers (Slack, PagerDuty, email)
- [ ] Webhook support for registries
- [ ] WebUI for monitoring and control
- [ ] Prometheus metrics export

## License

MIT

