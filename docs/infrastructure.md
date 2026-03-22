# OpenPriors Infrastructure

This file is the source of truth for which machines are in bounds for
OpenPriors work.

If a host is not documented here, do not assume it is safe to use for
OpenPriors deploys, debugging, or traffic cutovers.

## Boundary

OpenPriors infrastructure is separate from ExoPriors and Pivotality.

Do not use or modify:

- ExoPriors hosts
- ExoPriors repos
- Pivotality hosts
- Pivotality repos

If a task needs a new host, document that host here in the same change that
introduces it.

## Confirmed Dedicated Host

The confirmed OpenPriors-only machine is the inference gateway host.

- Public IPv4: `204.168.182.12`
- Hostname: `basin-openpriors-cluster-proxy`
- SSH user: `root`
- SSH key: `~/.ssh/basin-openpriors-cluster_ed25519`

Use the key documented here. Do not guess from similarly named local SSH keys.

Recommended SSH config:

```sshconfig
Host openpriors-inference
  HostName 204.168.182.12
  User root
  IdentityFile ~/.ssh/basin-openpriors-cluster_ed25519
  IdentitiesOnly yes
  StrictHostKeyChecking accept-new
```

Direct SSH example:

```bash
ssh -i ~/.ssh/basin-openpriors-cluster_ed25519 root@204.168.182.12
```

## Expected Remote State

This host should identify as `basin-openpriors-cluster-proxy`.

OpenPriors-specific state on that machine:

- systemd service: `openpriors-inference.service`
- app directory: `/opt/openpriors-inference/app`
- config directory: `/etc/openpriors-inference`
- health endpoint: `http://127.0.0.1:8088/healthz`
- listener: `0.0.0.0:8088`
- Caddy is not expected on this machine

Quick verification:

```bash
ssh openpriors-inference 'hostnamectl --static'
ssh openpriors-inference 'systemctl --no-pager --full status openpriors-inference.service | sed -n "1,40p"'
ssh openpriors-inference 'curl -fsS http://127.0.0.1:8088/healthz'
ssh openpriors-inference 'ss -tlnp | grep 8088'
```

Expected health response:

```json
{"ok":true,...}
```

## Public App Host

No standalone public OpenPriors app/API host is documented in this repo yet.

Do not infer one from other projects. Do not repurpose ExoPriors or Pivotality.

If a dedicated public app host is provisioned, add all of the following here
before using it:

- public IP or hostname
- machine hostname
- SSH user and key name
- deployed service names
- deployed filesystem paths
- reverse proxy entrypoint
- health checks

## Deployment Hygiene

Before any deploy or production debugging session:

1. Verify the target with `hostnamectl --static`.
2. Verify the expected service names exist.
3. Verify the expected filesystem paths exist.
4. Verify the health endpoint locally on the machine.
5. If any of those checks fail, stop and update this document before changing
   infrastructure.
