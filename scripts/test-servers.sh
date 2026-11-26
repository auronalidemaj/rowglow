#!/usr/bin/env bash
# Throwaway database servers for testing Rowglow's drivers, all in Docker:
# SQL Server, Redis (single, Cluster of 6 nodes, Sentinel), MongoDB and an
# SSH bastion. Cluster and Sentinel nodes announce 127.0.0.1 so this
# computer can reach every node through the published ports.
#   ./scripts/test-servers.sh up     start (credentials in scripts/.test-servers.env)
#   ./scripts/test-servers.sh down   stop and remove them
set -euo pipefail
cd "$(dirname "$0")"
ENV=.test-servers.env
NET=rowglow-test
if [ ! -f "$ENV" ]; then
  gen() { LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 20; }
  cat > "$ENV" <<VARS
MSSQL_SA_PASSWORD=Rg1!$(gen)
REDIS_PASSWORD=$(gen)
MONGO_USER=rowglow
MONGO_PASSWORD=$(gen)
SSH_USER=tunnel
SSH_PASSWORD=$(gen)
VARS
  chmod 600 "$ENV"
fi
# Added in 1.2.
grep -q '^SENTINEL_PASSWORD=' "$ENV" || {
  gen() { LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 20; }
  echo "SENTINEL_PASSWORD=$(gen)" >> "$ENV"
}
set -a; . "./$ENV"; set +a
ALL="rg-mssql rg-redis rg-mongo rg-ssh rg-redis-cluster rg-redis-sentinel"

case "${1:-up}" in
  up)
    docker network inspect $NET >/dev/null 2>&1 || docker network create $NET >/dev/null
    docker rm -f $ALL >/dev/null 2>&1 || true
    docker run -d --name rg-mssql --network $NET --platform linux/amd64 -p 1433:1433 \
      -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD="$MSSQL_SA_PASSWORD" -e MSSQL_PID=Developer \
      mcr.microsoft.com/mssql/server:2022-latest >/dev/null
    docker run -d --name rg-redis --network $NET -p 6379:6379 redis:7-alpine \
      redis-server --requirepass "$REDIS_PASSWORD" >/dev/null
    docker run -d --name rg-mongo --network $NET -p 27017:27017 \
      -e MONGO_INITDB_ROOT_USERNAME="$MONGO_USER" -e MONGO_INITDB_ROOT_PASSWORD="$MONGO_PASSWORD" mongo:7 >/dev/null
    docker run -d --name rg-ssh --network $NET -p 2222:2222 \
      -e PASSWORD_ACCESS=true -e USER_NAME="$SSH_USER" -e USER_PASSWORD="$SSH_PASSWORD" \
      -e DOCKER_MODS=linuxserver/mods:openssh-server-ssh-tunnel \
      lscr.io/linuxserver/openssh-server:latest >/dev/null
    # Redis Cluster: 3 primaries + 3 replicas on 7001-7006 (7000 is AirPlay on macOS).
    docker run -d --name rg-redis-cluster --network $NET -p 7001-7006:7001-7006 \
      -e PW="$REDIS_PASSWORD" redis:7-alpine sh -c '
        for p in 7001 7002 7003 7004 7005 7006; do
          mkdir -p /data/$p
          redis-server --port $p --dir /data/$p --cluster-enabled yes --cluster-config-file nodes.conf \
            --cluster-announce-ip 127.0.0.1 --requirepass "$PW" --masterauth "$PW" --appendonly no --daemonize yes
        done
        sleep 1
        redis-cli -a "$PW" --no-auth-warning --cluster create 127.0.0.1:7001 127.0.0.1:7002 127.0.0.1:7003 \
          127.0.0.1:7004 127.0.0.1:7005 127.0.0.1:7006 --cluster-replicas 1 --cluster-yes
        tail -f /dev/null' >/dev/null
    # Redis Sentinel: master :6390, replica :6391, Sentinel :26379 (master name "mymaster").
    docker run -d --name rg-redis-sentinel --network $NET -p 6390:6390 -p 6391:6391 -p 26379:26379 \
      -e PW="$REDIS_PASSWORD" -e SPW="$SENTINEL_PASSWORD" redis:7-alpine sh -c '
        redis-server --port 6390 --requirepass "$PW" --masterauth "$PW" --daemonize yes
        redis-server --port 6391 --replicaof 127.0.0.1 6390 --requirepass "$PW" --masterauth "$PW" --daemonize yes
        printf "port 26379\nsentinel monitor mymaster 127.0.0.1 6390 1\nsentinel auth-pass mymaster %s\nsentinel down-after-milliseconds mymaster 5000\nrequirepass %s\n" "$PW" "$SPW" > /data/sentinel.conf
        exec redis-sentinel /data/sentinel.conf' >/dev/null
    echo "started: rg-mssql :1433, rg-redis :6379, rg-mongo :27017, rg-ssh :2222,"
    echo "         rg-redis-cluster :7001-7006, rg-redis-sentinel :6390/:6391/:26379 (network $NET)"
    ;;
  down)
    docker rm -f $ALL >/dev/null 2>&1 || true
    docker network rm $NET >/dev/null 2>&1 || true
    echo "removed"
    ;;
esac
