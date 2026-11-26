#!/usr/bin/env bash
# Creates the Rowglow demo databases. Only touches databases named rowglow_demo.
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)/demo"
echo "→ PostgreSQL: rowglow_demo"
dropdb --if-exists rowglow_demo >/dev/null 2>&1 || true
createdb rowglow_demo
psql -q -v ON_ERROR_STOP=1 -d rowglow_demo -f "$DIR/postgres.sql" >/dev/null
MYSQL_BIN="${MYSQL_BIN:-$(command -v mysql)}"
if [ -x /opt/homebrew/opt/mysql@8.0/bin/mysql ]; then MYSQL_BIN=/opt/homebrew/opt/mysql@8.0/bin/mysql; fi
echo "→ MySQL: rowglow_demo"
"$MYSQL_BIN" -h127.0.0.1 -uroot -e "DROP DATABASE IF EXISTS rowglow_demo; CREATE DATABASE rowglow_demo CHARACTER SET utf8mb4;"
"$MYSQL_BIN" -h127.0.0.1 -uroot rowglow_demo < "$DIR/mysql.sql"
echo "→ SQLite: $DIR/music.sqlite"
rm -f "$DIR/music.sqlite"
sqlite3 "$DIR/music.sqlite" < "$DIR/sqlite.sql"
echo "Done."
