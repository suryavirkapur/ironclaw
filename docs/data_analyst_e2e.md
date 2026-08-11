# Firecracker data analyst E2E

This test gives the agent a root Ubuntu 24.04 microVM with direct network access. It does
not add a database-specific Rust tool or hard-code an analysis workflow: the agent installs
the clients and Python libraries it needs, connects over the network, queries data, creates
a chart, and publishes the result.

## Start the test database

```bash
./scripts/start-analyst-postgres.sh
```

This starts PostgreSQL 16 on host port `55432`, loads `tests/fixtures/analyst-sales.sql`,
and creates the login `analyst` with password `analyst-demo`. The role can read the demo
sales table but cannot create or mutate tables. The Firecracker guest reaches the host TAP
gateway at `172.16.0.1`.

## Run the analyst

Start the Firecracker daemon and CLI as described in [cli.md](cli.md), then ask:

```text
Install Python, pandas, psycopg2, seaborn and matplotlib if needed. Connect to PostgreSQL
at 172.16.0.1:55432, database ironclaw_analytics, as analyst. Query total revenue by
region, generate a bar chart, save it as /mnt/brain/workspace/revenue_by_region.png,
then publish the image.
```

Packages installed with `apt` remain in that analyst's private copy-on-write root disk.
The checksum-stamped Ubuntu base remains read-only, and another user receives a distinct
reflink disk with none of the analyst's later changes.

## What this security test proves

- Agent code and package installation execute as root inside a Firecracker microVM.
- The guest can reach an explicitly exposed host service through TAP/NAT.
- Database authorization remains effective: root in the guest is not PostgreSQL
  superuser, and the demo role rejects writes.
- Generated binary artifacts cross the vsock/channel boundary without granting the agent
  host filesystem access.

It does not prove hardened multi-tenant operation. The current direct-network profile
does not enforce domain allowlists, and the daemon is not yet run through Firecracker's
production jailer/cgroup setup.
