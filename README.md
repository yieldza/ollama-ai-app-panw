# panw-api-ollama

![file](https://github.com/user-attachments/assets/b54e6622-97e7-4ef8-8cd7-09dd2c5d89f3)

Enhance your Ollama deployment with enterprise-grade AI security using Palo Alto Networks Prisma AIRS AI Runtime API Intercept.

> **This fork ships a security review and a working production deployment recipe.**
> See [What's Fixed in This Fork](#whats-fixed-in-this-fork) for the full change set.

## What is this?

panw-api-ollama is a security proxy that sits between your [OpenWebUI](https://openwebui.com/) interface and [Ollama](https://ollama.com/) instance. It intercepts every prompt and response, scans them with Palo Alto Networks' Prisma AIRS AI Runtime, and protects against:

- Prompt injection attacks
- Data exfiltration attempts
- Harmful or toxic content
- PII leakage
- Other AI-specific security threats

Transparent to existing setups — Ollama works as before, with security scanning added.

## Why use this?

- **Prevent Security Incidents** — detect and block malicious prompts before they reach the LLM
- **Protect Sensitive Data** — ensure responses don't leak unauthorized information
- **Maintain Compliance** — implement guardrails for enterprise AI usage
- **Visibility** — gain insights into usage patterns and threats

## Use Cases

- Secure AI models in production
- Detect data poisoning before fine-tuning
- Protect against adversarial input/output
- Prevent sensitive data leakage via API-based threat detection

---

## What's Fixed in This Fork

This fork addresses **23 issues** found during a security review (14 code-level) and a production deployment dry-run (9 deployment-level). Every fix is in `main`; live-verified against PANW SG region + Ollama 0.24 + Open WebUI v0.9.5.

### 🔴 Critical — code

| # | Bug | Before | After |
|---|---|---|---|
| 1 | **Chat block loop** ([src/handlers/chat.rs](src/handlers/chat.rs)) | After PANW blocked a toxic prompt, every subsequent clean prompt in the same chat session also blocked. Required new chat to recover. Root cause: scan walked the whole history searching from the last `assistant` turn — if the client didn't append the blocked reply, the toxic message was re-scanned forever. | `scan_range()` scans only the last user message. Loop breaks regardless of client behavior. |
| 2 | **Stream race — unscanned content release** ([src/stream.rs](src/stream.rs)) | Assessment future cloned the buffer at length L1; new chunks extended it to L2 during in-flight scan. `commit()` marked L2 as "assessed" even though only L1 was scanned. Bytes leaked downstream un-scanned. | `InflightSnapshot { text_len, code_len, pending_count }` captured at `begin_assessment()`. `commit()` and `release_pending_chunks()` operate on the snapshot, never current length. |
| 3 | **process_stream_end pre-set last_assessed** ([src/stream.rs](src/stream.rs)) | `last_assessed_*` advanced BEFORE the final assessment future ran. If the future failed mid-flight, bookkeeping lied. | Snapshot flow applies — `last_assessed_*` advances only on success. |
| 4 | **Passthrough OpenAI/Anthropic bypass** ([src/handlers/passthrough.rs](src/handlers/passthrough.rs)) | `/v1/chat/completions`, `/v1/messages`, `/v1/embeddings`, `/v1/completions` flowed through passthrough to Ollama with NO PANW scan — full bypass of the proxy's purpose. | Block 4 known compat paths by default → HTTP 501. Opt-in with `PASSTHROUGH_COMPAT_UNSAFE=1`. |
| 5 | **Client auth header leak** ([src/ollama.rs](src/ollama.rs)) | Client `Authorization`, `Cookie`, `X-Api-Key` forwarded verbatim to Ollama — header smuggling / credential leak vector when proxy faces untrusted network. | `sanitize_forward_headers()` strips 6 credential headers + 9 hop-by-hop headers. |

### 🟠 High — code

| # | Bug | Fix |
|---|---|---|
| 6 | **Unbounded body size (OOM)** | `DefaultBodyLimit::max(10 MiB)` on scanned routes, `Limited::new(body, 200 MiB)` on passthrough, 4 MiB cap on PANW response stream. |
| 7 | **Multi-turn DLP mask persistence** | Documented as a known limitation (requires session state). |
| 8 | **Stream error silently 200 OK** | Emit terminal NDJSON frame with `done_reason: "error"` + Ollama-compatible shape so clients detect the error. |
| 9 | **Embeddings fake zero-vector on block** | Return HTTP **403** + JSON error body instead of a `vec![0.0; 10]` placeholder that corrupted downstream cosine similarity. |

### 🟡 Medium — code

| # | Bug | Fix |
|---|---|---|
| 10 | **ScanResponse `action` case-sensitive** | `action.eq_ignore_ascii_case("block")` — survives PANW casing changes (fail-closed). |
| 11 | **No graceful shutdown** | `axum::serve().with_graceful_shutdown()` on SIGINT/SIGTERM so in-flight streams flush. |
| 12 | **setup_logging after config load** | Init default tracing subscriber first; config errors are visible. |

### ⚪ Tech-debt cleanup

- Removed `convert_stream_error` no-op
- Removed stale commented `request.stream = Some(false)` in chat/generate handlers
- ~30 new inline regression tests covering the fixes above

### 🐛 Deployment fixes ([docker/docker-compose.yaml](docker/docker-compose.yaml), [Dockerfile](Dockerfile))

| Issue | Cause | Fix |
|---|---|---|
| ollama container immediately fails | `ollama/ollama:latest` changed entrypoint to `/bin/ollama`. The `command: > sh -c "..."` was parsed as `ollama sh -c "..."` → "unknown command sh" | Override `entrypoint: ["/bin/sh", "-c"]` + restructure command as a block scalar |
| ollama healthcheck always failing | `wget` no longer ships in the ollama image | Switch healthcheck to `["CMD", "/bin/ollama", "list"]` |
| panw-api-ollama (unhealthy) despite serving | Dockerfile HEALTHCHECK probed `localhost` — Alpine resolves to `::1` first, but the server binds `0.0.0.0` (IPv4 only) → ECONNREFUSED | Use `http://127.0.0.1:11435/healthz` in both Dockerfile and compose override |
| openclaw restart loop | Ships unconfigured by default — `Missing config. Run openclaw setup or set gateway.mode=local` | Set `OPENCLAW_GATEWAY_MODE=local` env |
| build fails: dockerfile not found | Compose pointed at `./docker/Dockerfile`; actual file at repo root | `dockerfile: ./Dockerfile` |
| `docker compose up` silently swaps local build for upstream `ghcr` image | Default `pull_policy` pulls latest | `pull_policy: never` on the `panw-api-ollama` service + `PANW_API_IMAGE=panw-api-ollama:local` in `.env` |
| Downstream services wait forever | `depends_on: condition: service_healthy` against a never-healthy proxy | `condition: service_started` |

### Verified end-to-end on production

- `/healthz` → `{"status":"ok","version":"0.17.0"}`
- POST `/api/chat` clean prompt → assistant reply
- POST `/api/chat` toxic prompt → `category=malicious, action=block`
- POST `/api/chat` `[user(toxic), user(clean)]` (no assistant in between) → prompt-side **allowed** (the original block-loop bug is gone); any subsequent harmful response is still blocked by PANW response-side scanning (correct defense-in-depth)

---

## Deployment

Two paths. Most operators want **Option A**.

### Option A — Docker Compose on a single host (recommended)

Works on any Linux distro, Apple Silicon, or Windows + WSL2. Steps below were validated on Ubuntu 22.04 + Docker 29.5.2 with PANW Singapore region.

#### Step 1 — Prerequisites

| Requirement | Notes |
|---|---|
| Docker Engine 24.0+ | Compose v2 plugin |
| ~15 GB disk | Most is the LLM weight file |
| 8 GB RAM (16 GB recommended) | More for larger models |
| linux/amd64 or linux/arm64 | Both supported |

#### Step 2 — Clone

```bash
git clone https://github.com/yieldza/ollama-ai-app-panw.git
cd ollama-ai-app-panw
```

#### Step 3 — Configure secrets

```bash
cd docker
cp .env.example .env
chmod 600 .env
```

Edit `docker/.env` and set the required values:

```env
SECURITY_API_KEY=<your-PANW-AI-Runtime-API-token>
SECURITY_PROFILE_NAME=<your-PANW-security-profile-name>
# Pick the region closest to you:
SECURITY_BASE_URL=https://service-sg.api.aisecurity.paloaltonetworks.com   # Singapore
# or https://service-de.api.aisecurity.paloaltonetworks.com                # Germany
# or https://service.api.aisecurity.paloaltonetworks.com                   # US (default)

# Recommended for production:
WEBUI_SECRET_KEY=<run: openssl rand -hex 32>
SECURITY_APP_USER=<your-org-or-team-name>
```

#### Step 4 — Pin local image (recommended)

If you want to deploy with the security fixes from this fork rather than the upstream registry image, add to `.env`:

```env
PANW_API_IMAGE=panw-api-ollama:local
```

`pull_policy: never` is already set in the compose file, so once a local image with that tag exists, `docker compose up` will never replace it.

#### Step 5 — Create OpenClaw workspace directory

OpenClaw bind-mounts a host directory. Create it before first start so Docker doesn't make it `root`-owned:

```bash
mkdir -p openclaw-workspace
```

#### Step 6 — Add your user to the docker group (if needed)

If `docker ps` fails with permission denied:

```bash
sudo usermod -aG docker $USER
# log out and back in (or open a new shell) for the group change to apply
```

#### Step 7 — Build the local image

```bash
# from the repo root
docker buildx create --name multi --driver docker-container --use   # one-time
docker buildx build --platform linux/amd64 --load -t panw-api-ollama:local -f Dockerfile .
```

> If you want to use the upstream registry image instead, **skip this step** and leave `PANW_API_IMAGE` unset in `.env`. You will not get the fixes from this fork.

Optionally, save the image as a tarball for restore-after-prune protection:

```bash
docker save panw-api-ollama:local | gzip > panw-api-ollama-local.tar.gz
```

Restore with:

```bash
docker load < panw-api-ollama-local.tar.gz
```

#### Step 8 — Start the stack

```bash
cd docker
docker compose -f docker-compose.yaml up -d
```

First boot takes 5–15 minutes — ollama downloads `llama2-uncensored:latest` (~3.8 GB) on its first run.

#### Step 9 — Verify

```bash
docker compose -f docker-compose.yaml ps
```

Expected (all healthy except openclaw which is optional):

```
NAME              STATUS
ollama            Up (healthy)
panw-api-ollama   Up (healthy)
open-webui        Up (healthy)
openclaw          Up (starting)        # optional
```

Probe the proxy from inside the network:

```bash
docker run --rm --network panw-api-ollama_default curlimages/curl:latest \
    -s http://panw-api-ollama:11435/healthz
# expect: {"status":"ok","version":"0.17.0"}
```

End-to-end chat:

```bash
docker run --rm --network panw-api-ollama_default curlimages/curl:latest \
    -s -X POST http://panw-api-ollama:11435/api/chat \
    -H 'Content-Type: application/json' \
    -d '{"model":"llama2-uncensored:latest","messages":[{"role":"user","content":"hello"}],"stream":false}'
```

#### Step 10 — Access Open WebUI

```
http://<host>:3000
```

First launch prompts you to create an admin account.

#### Step 11 — Restart resilience

All services use `restart: unless-stopped` and `docker.service` is `enabled` by default on most distros — the stack auto-starts after host reboot. Combined with `pull_policy: never` and the tarball backup, your local image survives reboots, recreates, and accidental prunes.

---

### Option B — Build from source (no Docker)

For development or systems that can't run Docker.

```bash
git clone https://github.com/yieldza/ollama-ai-app-panw.git
cd ollama-ai-app-panw
cargo build --release
```

Configure:

```bash
cp config.yaml.exemple config.yaml
# edit config.yaml — set api_key, profile_name, base_url, ollama base_url
```

Run:

```bash
./target/release/panw-api-ollama
```

Point your OpenWebUI Ollama base URL at `http://localhost:11435` instead of `:11434`.

---

## Configuration

### Required PANW values

| Field | env var | YAML key | Notes |
|---|---|---|---|
| API key | `SECURITY_API_KEY` | `security.api_key` | From <https://aisecurity.paloaltonetworks.com> |
| Profile name | `SECURITY_PROFILE_NAME` | `security.profile_name` | The security profile to scan against |
| Base URL | `SECURITY_BASE_URL` | `security.base_url` | Region-specific endpoint |

### Optional runtime guards added in this fork

| env var | Default | Effect |
|---|---|---|
| `PASSTHROUGH_DISABLED` | unset | Set `1` to disable the catch-all passthrough entirely |
| `PASSTHROUGH_COMPAT_UNSAFE` | unset | Set `1` to allow `/v1/chat/completions`, `/v1/messages`, `/v1/embeddings`, `/v1/completions` to passthrough UNSCANNED (not recommended) |
| `STREAM_CHUNK_TIMEOUT_SECS` | 300 | Per-chunk wall-clock timeout for upstream Ollama streams |

### Open WebUI prefix routing example

`config-examples/config-1747909231428.json` shows how to register two Ollama "providers" — one scanned, one bypassing — so you can A/B test in the same UI:

```json
{
    "ollama": {
        "base_urls": [
            "http://panw-api-ollama:11435",
            "http://host.docker.internal:11434"
        ],
        "api_configs": {
            "0": { "enable": true, "prefix_id": "PANW",   "model_ids": ["llama2-uncensored:latest"] },
            "1": { "enable": true, "prefix_id": "NOPAWN", "model_ids": ["llama2-uncensored:latest"] }
        }
    }
}
```

Two example model configurations are included:

- `PANW.llama2-uncensored_latest-*.json` — through the proxy
- `NOPAWN.llama2-uncensored_latest-*.json` — bypassing the proxy

---

## Tests

```bash
cargo test                                    # ~30 inline unit + regression tests
cargo test --test smoke -- --ignored          # end-to-end smoke (needs live proxy + ollama)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Smoke tests cover every endpoint with benign and malicious payloads, streaming and non-streaming, across `/api/chat`, `/api/generate`, `/api/embed`, and `/api/embeddings`. They are gated behind `#[ignore]` so `cargo test` stays fast and offline.

Run against a live proxy:

```bash
BASE_URL=http://localhost:11435 \
  MODEL=llama2-uncensored:latest \
  cargo test --test smoke -- --ignored --nocapture
```

Key tests to confirm a clean deploy:

- `healthz_returns_ok_and_version` — proxy liveness
- `clean_followup_after_blocked_turn_is_allowed` — regression for the persistent-block bug
- `chat_non_stream_malicious_blocked_or_masked` — PANW blocking works

---

## Production hardening checklist

Before exposing this to real users:

- [ ] `WEBUI_SECRET_KEY` set to a random 32-byte hex string (`openssl rand -hex 32`)
- [ ] `SECURITY_APP_USER` set to a stable identifier (used in PANW audit logs)
- [ ] `.env` mode is `600` and not committed
- [ ] Reverse proxy (nginx / Caddy / Traefik) with TLS in front of port 3000
- [ ] Security group inbound restricted to a known IP / VPN range — NOT `0.0.0.0/0`
- [ ] `PANW_API_IMAGE` pinned to a specific digest or local tag
- [ ] Healthcheck thresholds reviewed for your traffic pattern
- [ ] Backup strategy for the `open-webui` volume (user accounts + chat history)
- [ ] `openclaw-workspace/` access restricted appropriately
- [ ] API key rotation policy in place (treat any key that appeared in plaintext as compromised)

---

## Known limitations

1. **DLP mask persistence across turns** — prior-turn user messages with PII are re-sent unmasked to Ollama by the client. Fixing properly requires per-session state, which this proxy deliberately does not keep. See the comment in `src/handlers/chat.rs`.
2. **Multi-message user turns** — `scan_range` scans only the last user message; multi-part user turns before any assistant reply have intermediate messages skipped. Documented tradeoff against the block-loop UX bug.
3. **Response-side block UX** — when a prior turn contained toxic content, Ollama may still generate harmful output from that context, which PANW correctly blocks at the response side. This can feel like "still blocking" even though the prompt itself passed.
4. **Mock-based integration tests not yet added** — Task #13 in the review was deferred; `tests/smoke.rs` remains `#[ignore]` and requires a live stack.

---

## Resources

- [Product Information](https://www.paloaltonetworks.com/prisma/prisma-ai-runtime-security)
- [Documentation](https://docs.paloaltonetworks.com/ai-runtime-security)
- [API Reference](https://pan.dev/prisma-airs/scan/api/)
- [Detailed Docker deployment guide](docker/DEPLOYMENT.md)

---

## Support

For issues with this fork, file a GitHub issue on the repo. For questions about Palo Alto Networks Prisma AIRS AI Runtime API Intercept, refer to official Palo Alto Networks support channels.

For the full per-commit change log of fixes in this fork, run:

```bash
git log --oneline main
```

Current state:

```
0d9b138  healthcheck: use 127.0.0.1 instead of localhost (IPv6 mismatch)
8ad65c4  docker-compose: pin local image via pull_policy: never
20e33d7  docker-compose: fixes for production deployment
bc87cd8  Initial commit: panw-api-ollama with security review fixes (14 issues)
```
