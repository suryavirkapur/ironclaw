#!/usr/bin/env bash
set -euo pipefail

container_name="${IRONCLAW_POSTGRES_CONTAINER:-ironclaw-analyst-postgres}"
host_port="${IRONCLAW_POSTGRES_PORT:-55432}"

if docker inspect "${container_name}" >/dev/null 2>&1; then
  docker start "${container_name}" >/dev/null
else
  docker run --detach \
    --name "${container_name}" \
    --publish "${host_port}:5432" \
    --env POSTGRES_DB=ironclaw_analytics \
    --env POSTGRES_PASSWORD=admin-demo \
    --mount \
      "type=bind,src=$(pwd)/tests/fixtures/analyst-sales.sql,dst=/docker-entrypoint-initdb.d/analyst-sales.sql,readonly" \
    postgres:16-alpine >/dev/null
fi

for attempt in $(seq 1 30); do
  if docker exec "${container_name}" \
    pg_isready --username postgres --dbname ironclaw_analytics >/dev/null 2>&1; then
    echo "Postgres ready on 0.0.0.0:${host_port} (database ironclaw_analytics)"
    exit 0
  fi
  sleep 1
done

echo "Postgres did not become ready" >&2
exit 1
