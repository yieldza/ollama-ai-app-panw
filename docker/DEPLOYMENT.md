# Deployment Guide — panw-api-ollama stack

End-to-end steps to deploy the full stack (Ollama + PANW security proxy + OpenClaw + Open WebUI v0.9.5) on **any Linux distribution**, Windows with WSL2, or Apple Silicon macOS.

The container image is built as a fully statically-linked musl binary running on Alpine, so it works the same on Ubuntu, Debian, RHEL, Fedora, Alpine, Amazon Linux, openSUSE, Arch, etc.

---

## 1. Prerequisites

| Requirement | Minimum version | Notes |
|---|---|---|
| Docker Engine | 24.0+ | Compose v2 plugin included by default |
| docker compose | v2.20+ | Run `docker compose version` to check |
| Disk space | ~15 GB | Most of it is the LLM weight file pulled by Ollama |
| RAM | 8 GB | 16 GB recommended for larger models |
| Architecture | linux/amd64 or linux/arm64 | Both fully supported |

Optional:

- **NVIDIA GPU** — install [nvidia-container-toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) on the host if you want GPU acceleration (use `docker-compose.win.yaml`).
- **Apple Silicon** — install native [Ollama](https://ollama.com/download) on macOS for full Neural Engine acceleration (use `docker-compose.apple.yaml`).

---

## 2. Get the code

```bash
git clone https://github.com/PaloAltoNetworks/panw-api-ollama.git
cd panw-api-ollama
```

> If you already have the source, just `cd` into the project root.

---

## 3. Configure environment variables

All configuration lives in **one file**: `docker/.env`. The compose files automatically load it.

```bash
cd docker
cp .env.example .env
```

Edit `docker/.env` and set the two **required** values:

```env
SECURITY_API_KEY=<your-PANW-AI-Runtime-API-token>
SECURITY_PROFILE_NAME=<your-PANW-security-profile-name>
```

> Obtain both from <https://aisecurity.paloaltonetworks.com>.

Optional values worth setting in production:

```env
WEBUI_SECRET_KEY=<run: openssl rand -hex 32>
SECURITY_APP_USER=<your-org-or-team-name>
SERVER_DEBUG_LEVEL=INFO          # or DEBUG for troubleshooting
OPEN_WEBUI_PORT=3000             # change if 3000 is already in use
```

---

## 4. Create the OpenClaw workspace directory

OpenClaw bind-mounts a host directory for file read/write. Create it before first start, otherwise Docker will create it as `root:root` and openclaw won't be able to write to it.

```bash
# from the docker/ directory
mkdir -p openclaw-workspace
```

---

## 5. Pick the right compose file

| File | When to use |
|---|---|
| `docker-compose.yaml` | **Default for all Linux distros** (Ubuntu, Debian, RHEL, Alpine, Fedora, …) — CPU only |
| `docker-compose.win.yaml` | Windows host with WSL2 + NVIDIA GPU |
| `docker-compose.apple.yaml` | macOS on Apple Silicon (M1/M2/M3/M4) with native Ollama |

The rest of this guide uses `docker-compose.yaml`. Swap the filename in the commands if you picked a different one.

---

## 6. Build the proxy image

If you only want to **use the published image** from `ghcr.io/paloaltonetworks/panw-api-ollama:latest`, **skip this step** — `docker compose up` will pull it automatically.

To build locally (e.g. you modified the code):

### Single-arch local build (fastest, matches your host)

```bash
docker compose -f docker-compose.yaml build panw-api-ollama
```

### Multi-arch build for both amd64 + arm64

Requires `docker buildx` (bundled with Docker Desktop and Engine 23.0+).

```bash
# One-time: create a buildx builder that supports multi-platform.
docker buildx create --name multi --driver docker-container --use
docker buildx inspect --bootstrap

# Cross-compile for both architectures and load locally (amd64 only)
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -t panw-api-ollama:local \
  -f ../Dockerfile \
  ..

# Or push to a registry
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -t ghcr.io/your-org/panw-api-ollama:dev \
  --push \
  -f ../Dockerfile \
  ..
```

The Dockerfile cross-compiles natively via `tonistiigi/xx` — **no QEMU emulation**, so arm64 builds on an amd64 host take ~3-5 minutes instead of ~25.

---

## 7. Start the stack

```bash
docker compose -f docker-compose.yaml up -d
```

You'll see four containers come up in dependency order:

```
[+] Running 5/5
 ✔ Network panw-api-ollama_default        Created
 ✔ Container ollama                       Healthy
 ✔ Container panw-api-ollama              Healthy
 ✔ Container openclaw                     Started
 ✔ Container open-webui                   Started
```

> First boot takes 1-5 minutes because Ollama downloads `llama2-uncensored:latest` (~3.8 GB) on startup.

---

## 8. Verify the deployment

### 8.1 Check container status

```bash
docker compose -f docker-compose.yaml ps
```

All four services should report `Up` and `(healthy)`:

```
NAME              STATUS                  PORTS
ollama            Up 2 min (healthy)
panw-api-ollama   Up 2 min (healthy)
openclaw          Up 2 min                0.0.0.0:18789->18789/tcp
open-webui        Up 2 min                0.0.0.0:3000->8080/tcp
```

If a service shows `(unhealthy)` or `(starting)` for more than 5 minutes, see [§10 Troubleshooting](#10-troubleshooting).

### 8.2 Hit the proxy's `/healthz` directly

The proxy doesn't expose its port to the host by default, so probe it from inside the network:

```bash
docker compose -f docker-compose.yaml exec panw-api-ollama \
  wget -qO- http://localhost:11435/healthz
```

Expected output:

```json
{"status":"ok","version":"0.17.0"}
```

### 8.3 End-to-end chat test

```bash
docker compose -f docker-compose.yaml exec panw-api-ollama \
  wget -qO- --post-data='{"model":"llama2-uncensored:latest","messages":[{"role":"user","content":"Reply with the single word: hello."}],"stream":false}' \
  --header='Content-Type: application/json' \
  http://localhost:11435/api/chat
```

You should get back a JSON envelope with `"role":"assistant"` and a short reply.

### 8.4 Run the smoke test suite (optional)

If you have Rust installed on the host:

```bash
# Briefly expose the proxy port (Ctrl-C when done)
docker compose -f docker-compose.yaml run --rm -p 11435:11435 panw-api-ollama &

BASE_URL=http://localhost:11435 \
MODEL=llama2-uncensored:latest \
cargo test --test smoke -- --ignored --nocapture
```

Key tests to confirm:

- `healthz_returns_ok_and_version` — proxy liveness
- `clean_followup_after_blocked_turn_is_allowed` — regression test for the persistent-block bug
- `chat_non_stream_malicious_blocked_or_masked` — PANW blocking works

---

## 9. Access the services

| Service | URL | Notes |
|---|---|---|
| **Open WebUI** | <http://localhost:3000> | Main chat UI |
| **OpenClaw** | <http://localhost:18789> | Agent gateway |
| **PANW proxy** | not exposed | Reached only inside the Docker network |
| **Ollama** | not exposed | Reached only via the PANW proxy |

First launch of Open WebUI prompts you to create an admin account. Once logged in, the model dropdown should already list `llama2-uncensored:latest` — Open WebUI talks to `http://panw-api-ollama:11435` which forwards (security-scanned) traffic to Ollama.

---

## 10. Troubleshooting

### Containers stuck in `starting` state

```bash
docker compose -f docker-compose.yaml logs --tail=100 -f
```

Common causes:

| Symptom | Fix |
|---|---|
| `ollama` keeps downloading | Wait — first boot pulls ~3.8 GB. Increase `start_period` if your network is slow. |
| `panw-api-ollama` health check fails with `401` | `SECURITY_API_KEY` in `.env` is missing/wrong |
| `panw-api-ollama` exits with config error | `SECURITY_PROFILE_NAME` is missing |
| `open-webui` 502s | `panw-api-ollama` not healthy yet — wait or check its logs |
| `openclaw` permission denied | `openclaw-workspace/` owned by root — `sudo chown -R 1000:1000 openclaw-workspace` |

### Persistent block after a flagged message

This was the bug fixed in v0.17.0. If you see it again:

1. Confirm the image tag actually contains the fix: `docker compose images panw-api-ollama`
2. Check the unit test passes: `cargo test handlers::chat::tests::second_turn_after_blocked_skips_history`
3. Verify by sending two requests — first malicious, then clean follow-up containing the full history. See [tests/smoke.rs](../tests/smoke.rs) `clean_followup_after_blocked_turn_is_allowed`.

### Image won't build for arm64

Check the buildx builder is multi-arch:

```bash
docker buildx ls
```

The active builder must list `linux/amd64*, linux/arm64*` in its platforms. If not:

```bash
docker buildx create --name multi --driver docker-container --use
docker buildx inspect --bootstrap
```

### Reset everything

```bash
docker compose -f docker-compose.yaml down -v   # also removes volumes (model cache!)
rm -rf openclaw-workspace
```

---

## 11. Upgrading

### Upgrade Open WebUI to a newer release

Edit `docker/.env`:

```env
WEBUI_DOCKER_TAG=v0.9.6
```

Then:

```bash
docker compose -f docker-compose.yaml pull open-webui
docker compose -f docker-compose.yaml up -d open-webui
```

### Upgrade the PANW proxy image

```bash
docker compose -f docker-compose.yaml pull panw-api-ollama
docker compose -f docker-compose.yaml up -d panw-api-ollama
```

### Upgrade Ollama

```bash
docker compose -f docker-compose.yaml pull ollama
docker compose -f docker-compose.yaml up -d ollama
```

> Model files in the `ollama` volume survive container restarts — no re-download needed.

---

## 12. Production checklist

Before exposing this to real users:

- [ ] `WEBUI_SECRET_KEY` set to a random 32-byte hex string (`openssl rand -hex 32`)
- [ ] `SECURITY_APP_USER` set to a stable identifier (used in PANW audit logs)
- [ ] `.env` mode is `600` and not committed to git
- [ ] Reverse proxy (nginx / Caddy / Traefik) with TLS in front of port 3000
- [ ] Image pinned by digest, not `:latest`, in `PANW_API_IMAGE`
- [ ] Healthcheck thresholds reviewed for your traffic pattern
- [ ] Log shipping configured (`docker compose logs -f` is not a logging strategy)
- [ ] Backup strategy for the `open-webui` volume (contains user accounts and chat history)
- [ ] `openclaw-workspace/` access is restricted appropriately (the agent has read/write here)
