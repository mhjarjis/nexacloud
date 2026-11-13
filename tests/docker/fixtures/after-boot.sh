#!/usr/bin/env bash
# Run once systemd is up: create sample databases with a little data.
set -euo pipefail

for i in $(seq 1 60); do
  mysqladmin ping >/dev/null 2>&1 && break
  sleep 1
done
mysql -e "CREATE DATABASE IF NOT EXISTS shop_prod; CREATE TABLE IF NOT EXISTS shop_prod.orders (id INT PRIMARY KEY AUTO_INCREMENT, total DECIMAL(10,2)); INSERT INTO shop_prod.orders (total) VALUES (10.50),(99.00);"
mysql -e "CREATE DATABASE IF NOT EXISTS portal_db; CREATE TABLE IF NOT EXISTS portal_db.pages (id INT PRIMARY KEY, title VARCHAR(100));"

for i in $(seq 1 60); do
  runuser -u postgres -- psql -Atc 'select 1' >/dev/null 2>&1 && break
  sleep 1
done
runuser -u postgres -- psql -Atc "SELECT 1 FROM pg_database WHERE datname='api_prod'" | grep -q 1 \
  || runuser -u postgres -- createdb api_prod
runuser -u postgres -- psql -d api_prod -c "CREATE TABLE IF NOT EXISTS events (id serial primary key, body text); INSERT INTO events (body) VALUES ('hello');" >/dev/null
echo "fixtures ready"
