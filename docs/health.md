# OpenPriors Health Bar

This document defines what "healthy" means for the OpenPriors repo and API
server. It is the explicit bar for changes, deploys, and operational work.

## Focal Object

OpenPriors repo plus the Rust API server in this repository.

## Phase

Launch and operations.

## Decision Frame

Ship with guardrails. Great health means the automated gates are green, the
service-level checks are green, and the remaining risks are known rather than
implicit.

## Properties We Want

| Property family | Great health means | Current mechanism / evidence | Main failure mode | Ongoing bar |
| --- | --- | --- | --- | --- |
| Changeability | One engineer can change the system safely without hidden coupling. | Small axum service, explicit modules, CI runs fmt/test/clippy, infra boundary is documented. | Silent regressions or local-only fixes that do not survive push. | Every push passes CI and updates docs when behavior changes. |
| State integrity | Core invariants are enforced by schema and write paths. | Canonical entity ordering, derived scores, append-only credits, schema checks. | Drift between judgements, comparisons, and scores; invalid ordering; negative balances. | Keep invariants at DB and API boundaries, not in comments alone. |
| Provenance and auditability | Operators can explain what happened and why. | Judgements store prompt, reasoning, raw output, cost, latency. Infra hosts are documented. Request IDs are propagated on every response. | Incidents without enough context to reconstruct inputs or ownership. | Preserve full traces and keep host/service docs current. |
| Reliability and failure containment | The service degrades clearly and shuts down cleanly. | Structured liveness and readiness endpoints, DB readiness check, graceful shutdown, bounded body size. | Deploys drop in-flight work or route traffic to an instance with a dead DB path. | `healthz` and `readyz` must stay accurate and cheap. |
| Security and abuse resistance | Secrets, admin actions, and public data exposure are tightly scoped. | API key scopes, admin IP allowlist, public judgements default off, security headers. | Over-broad access, accidental trace exposure, or unsafe deploy shortcuts across projects. | Keep defaults closed and document any new trust boundary. |
| Performance and cost | Capacity and provider cost remain bounded and legible. | Metered gateway, credit ledger, configurable DB pool sizing, solver-derived writes. | Runaway LLM spend or DB contention under load. | Tune pool limits intentionally and keep expensive paths visible. |
| Operability and recoverability | Operators have a short, repeatable path to verify and recover the service. | `healthz`, `readyz`, infrastructure doc, local verification commands. | Recovery depends on oral history or shell archaeology. | Every deploy path should have a written verification loop. |
| Governance and maintainership | Repo state, ownership boundaries, and release checks are explicit. | Dedicated OpenPriors infra boundary, CI workflow, AGENTS guidance. | Cross-project confusion, unreviewable changes, or "works on my machine" drift. | Main stays releasable; infra changes are documented in-repo. |

## Release Gates

These are the minimum repo-level checks before merging or pushing operational
changes:

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Local Service Checks

With the app running locally:

```bash
curl -fsS http://localhost:8080/healthz
curl -fsS http://localhost:8080/readyz
```

Expected `healthz` response shape:

```json
{"ok":true,"service":"openpriors","version":"<crate-version>"}
```

Expected `readyz` response shape when the DB path is healthy:

```json
{"ok":true,"service":"openpriors","version":"<crate-version>","checks":{"database":"ok"}}
```

## Remote Verification

For the currently documented OpenPriors-only host, use the checks in
[docs/infrastructure.md](./infrastructure.md) before changing anything on the
machine.

## Known Remaining Gaps

These are still real risks even after the current health pass:

- No dedicated public OpenPriors app host is documented yet. Do not infer one.
- There is no metrics or alerting pipeline documented in this repo yet.
- Backup and restore procedure documentation is still missing.
- Migration rollout and rollback procedure documentation is still missing.

Those gaps should be closed before calling the production story fully mature.
