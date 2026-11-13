# Database manager (Phase 2 MySQL/MariaDB; Phase 3 PostgreSQL parity)

Conventions are in [README.md](README.md). `{engine}` is `mysql` (which includes
MariaDB) or `postgres`. Every `/api/db/postgres/...` endpoint returns
`501 {"error":"PostgreSQL support arrives in v0.4","code":"not_implemented"}` until P3.
The v0.1 endpoints `GET /api/databases` and `POST /api/databases/{engine}/{name}/backup`
are unchanged.

**Drivers:**
- MySQL: `mysql_async` over the unix socket `/run/mysqld/mysqld.sock`.
- PostgreSQL: `tokio-postgres` over TCP `127.0.0.1:5432`, because the default `pg_hba`
  allows scram on loopback TCP.

CLI tools (`mysqldump`, `mysql`, `pg_dump`, `psql`) are used only for dump, restore and
import. Their credentials come from defaults files or stdin (README §1.12).

**Identifiers:**
- A database, table or column name from the URL or body must **exactly equal** a name
  returned by `information_schema` for that request. Otherwise the response is `404`.
- Names are quoted only after that check: MySQL backticks with `` ` `` doubled; Postgres
  `"` with `"` doubled.
- Values are always bound as parameters.

**Sudo baseline.** Every class C or D `apply` needs sudo (README §1.9, following the
Phase 0 precedent). On preview/apply rows, the `Sudo` column marks whether the
**preview** also needs sudo. On J and S rows, it marks the endpoint itself.

## 1. Value encoding (rows, query results)
| SQL value | JSON |
|---|---|
| NULL | `null` |
| integer within ±2^53 | number |
| larger integer, DECIMAL | string |
| float | number. `NaN`/`Inf` become strings. |
| text (valid UTF-8, ≤ 64 KiB) | string |
| text > 64 KiB | `{"$truncated":true,"preview":"<first 1 KiB>","bytes":n}` |
| binary / non-UTF-8 | `{"$b64":"…","bytes":n}` (≤ 64 KiB, else truncated as above) |
| date / time / datetime / timestamp | the server's string, e.g. `"2026-11-13 03:00:00"` |
| JSON column | string (raw JSON text) |

Writes accept the same encodings. They also accept `{"$default":true}`, which means use
the column DEFAULT.

## 2. Admin users

**MySQL (P2).** Two users are provisioned by the `mysql` catalog install, by the
endpoint below, or lazily on first DB-manager use.

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| GET | `/api/db/{engine}/admin-user` | – | – | – |
| POST | `/api/db/{engine}/admin-user/preview` / `apply` | C | ✓ | `db-server:{engine}` |

`GET` returns `{"admin":"ok|missing|broken","ro":"ok|missing|broken","auth":"socket_root|password"}`.

The body is `{"root_password":null}`. It is needed only when root does not use
`auth_socket`/`unix_socket`, and it is written to a temp defaults file, never argv.

Passwords are generated at apply (README rule R1). SQL is sent on **stdin** to
`mysql --defaults-extra-file=<root or temp cnf>
--batch`. Passwords are 32 chars from `[A-Za-z0-9]`, generated server-side, so they are
literal-safe.
```sql
CREATE USER IF NOT EXISTS 'nexacloud_admin'@'localhost' IDENTIFIED BY '<pw1>';
ALTER USER 'nexacloud_admin'@'localhost' IDENTIFIED BY '<pw1>';
GRANT ALL PRIVILEGES ON *.* TO 'nexacloud_admin'@'localhost' WITH GRANT OPTION;
CREATE USER IF NOT EXISTS 'nexacloud_ro'@'localhost' IDENTIFIED BY '<pw2>';
ALTER USER 'nexacloud_ro'@'localhost' IDENTIFIED BY '<pw2>';
GRANT SELECT, SHOW VIEW, SHOW DATABASES, PROCESS ON *.* TO 'nexacloud_ro'@'localhost';
```
The credentials are written to these files (0600 root):
- `/etc/nexacloud/secrets/mysql-admin.cnf`:
  ```ini
  [client]
  user=nexacloud_admin
  password=<pw1>
  socket=/run/mysqld/mysqld.sock
  ```
- `/etc/nexacloud/secrets/mysql-ro.cnf`, the same with `nexacloud_ro`.

Rules:
- **Verify:** connect with both users, then `SELECT CURRENT_USER()`.
- **Diff:** the change's file diffs are `secret:true`.
- **Rollback:** `DROP USER` for any user that was created, and restore the cnf files.
- **v0.1 listing and dumps** switch to `mysql-admin.cnf` when it is present, and fall
  back to root socket auth otherwise.

**PostgreSQL (P3).** SQL is sent on stdin to
`runuser -u postgres -- psql -v ON_ERROR_STOP=1 -X -q`:
```sql
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='nexacloud_admin') THEN CREATE ROLE nexacloud_admin LOGIN; END IF; END $$;
ALTER ROLE nexacloud_admin WITH LOGIN CREATEDB CREATEROLE PASSWORD '<pw1>';
GRANT pg_read_all_data, pg_write_all_data, pg_monitor TO nexacloud_admin;   -- PG ≥ 14
DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='nexacloud_ro') THEN CREATE ROLE nexacloud_ro LOGIN; END IF; END $$;
ALTER ROLE nexacloud_ro WITH LOGIN PASSWORD '<pw2>';
GRANT pg_read_all_data, pg_monitor TO nexacloud_ro;
```
The credentials go to `/etc/nexacloud/secrets/pg-admin.pgpass` and `pg-ro.pgpass` in
`.pgpass` format: `127.0.0.1:5432:*:<user>:<pw>`.

## 3. Browse (read-only, no sudo)

| Method | Path | Phase |
|---|---|---|
| GET | `/api/db/{engine}/overview` | P2 |
| GET | `/api/db/{engine}/databases/{db}/tables` | P2 |
| GET | `/api/db/{engine}/databases/{db}/tables/{table}` | P2 |
| GET | `/api/db/{engine}/databases/{db}/tables/{table}/rows` | P2 |

For Postgres, `?schema=public` (the default) selects the schema.

**Overview.**
```json
{"engine":"mysql","flavor":"mysql|mariadb","version":"8.4.3","title":"MySQL 8.4 · 5 databases",
 "stats":{"databases":5,"total_bytes":2254857830,"connections":23,"max_connections":151,"cache_hit_rate":0.992},
 "databases":[{"name":"shop_prod","bytes":1288490188,"tables":84,"users":1,"used_by":["shop.nexadev.io"],"last_backup":{"at":"…","status":"ok"}}],
 "admin_user":"ok"}
```

| Field | MySQL | PostgreSQL |
|---|---|---|
| `connections` | `Threads_connected` | `sum(numbackends)` from `pg_stat_database` |
| `max_connections` | `@@max_connections` | `max_connections` |
| `cache_hit_rate` | `1 − Innodb_buffer_pool_reads / Innodb_buffer_pool_read_requests` | `Σblks_hit / (Σblks_hit + Σblks_read)` |
| `bytes` | `Σ(DATA_LENGTH+INDEX_LENGTH)` from `information_schema.TABLES` | `pg_database_size` |
| `users` | distinct non-system users with grants on the database | – |

Two fields come from outside the database server:
- `used_by`: projects whose `.env` has a matching `DB_DATABASE` and engine.
- `last_backup`: the newest backup artifact for `db:<engine>/<name>`.

System schemas are excluded, as in v0.1.

**Tables.**
```json
[{"name":"contacts","type":"table|view","engine":"InnoDB","rows_estimate":120400,"data_bytes":…,"index_bytes":…,"collation":"utf8mb4_0900_ai_ci","comment":"","primary_key":["id"],"editable":true}]
```
`editable` is true when the table has a primary key, or a unique index whose columns
are all NOT NULL. Views are never editable.

**Table detail.**
```json
{"columns":[{"name":"email","type":"varchar(255)","nullable":false,"default":null,"key":"UNI","extra":"","comment":"","position":3}],
 "indexes":[{"name":"contacts_email_unique","columns":["email"],"unique":true,"type":"BTREE"}],
 "foreign_keys":[{"name":"contacts_company_id_foreign","columns":["company_id"],"ref_table":"companies","ref_columns":["id"],"on_delete":"CASCADE"}],
 "create_sql":"CREATE TABLE `contacts` (…)"}
```
Sources: `information_schema.COLUMNS`, `STATISTICS`, `KEY_COLUMN_USAGE` and
`REFERENTIAL_CONSTRAINTS`, plus `SHOW CREATE TABLE`.

**Rows.** `?page=1&per_page=50&sort=email&dir=asc&filters=<urlencoded JSON>`

| Param | Rule |
|---|---|
| `page` | ≥ 1 |
| `per_page` | 10–500, default 50 |
| `sort` | an existing column. Default: the PK, ascending. |
| `dir` | `asc` \| `desc` |
| `filters` | JSON array, ≤ 10 entries, AND-combined: `[{"col":"email","op":"like","value":"%@gmail.com"}]` |

Filter `op` values:
- `eq`, `ne`, `lt`, `le`, `gt`, `ge`
- `like`, `nlike`
- `null`, `notnull` (no `value`)
- `in` (`value` is an array, ≤ 100 items)

The query is
`SELECT * FROM <db>.<t> WHERE … ORDER BY <sort> <dir>, <pk> LIMIT ? OFFSET ?`, on the
**ro** connection with a 10 s timeout. The response:
```json
{"columns":[{"name":"id","type":"bigint unsigned"}],"rows":[[17,"a@b.c"]],"pk":["id"],"page":1,"per_page":50,"total":120400,"total_exact":false}
```
`total` is an exact `COUNT(*)` (5 s timeout) when `rows_estimate < 1,000,000` and no
filters are set, or when filters are set and the count finishes within 5 s. Otherwise it
is the estimate, with `total_exact:false`.

## 4. Edit, insert and delete by primary key (class D)

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/db/{engine}/databases/{db}/tables/{table}/rows/preview` | D | ✓ | – |
| POST | `/api/db/{engine}/databases/{db}/tables/{table}/rows/apply` | D | ✓ | none taken. Refused with `409 resource_busy` while a restore, import or drop job holds `db:<engine>/<db>`. |

Bodies:
```json
{"op":"update","pk":{"id":17},"set":{"email":"new@b.c"},"expect":{"email":"old@b.c"}}
{"op":"insert","values":{"email":"x@b.c","name":"X"}}
{"op":"delete","pk":{"id":17},"expect":{"email":"old@b.c"}}
```
**Validation:**
- The table must be `editable`, else `409 conflict` ("no primary key — use the SQL
  console").
- `pk` must name exactly the key columns.
- `set`, `values` and `expect` keys must be existing columns.
- Generated or virtual columns are rejected.

**Preview:**
1. `SELECT * … WHERE <pk> FOR SHARE`. Exactly one row must match, else `404`.
2. If `expect` differs from the current row, the response is `409 stale_plan` ("row
   changed since you loaded it").
3. The response:
   ```json
   {"kind":"db_row_update","plan_hash":"…","target":"db:mysql/crm_prod","title":"Update contacts #17",
    "sql":"UPDATE `crm_prod`.`contacts` SET `email` = ? WHERE `id` = ? LIMIT 1","params":["new@b.c",17],
    "before":{"id":17,"email":"old@b.c"},"affected_estimate":1,"reversible":true}
   ```

**Apply:** the same body plus `plan_hash`. The server rebuilds the plan, re-reading the row; if the row changed, the hash differs and the response is `409`.
1. In one transaction: `SELECT … FOR UPDATE`, and compare the row with `before` by
   hash. A mismatch gives `409 stale_plan`.
2. Execute. The affected row count must be exactly 1, else `ROLLBACK` and `409 conflict`.
3. `COMMIT`. The response is `200 {"change_id","plan_hash","result":{"affected":1,"insert_id":null,"row":{…after…}}}`.

**Rollback** (Phase 0 `POST /api/changes/{id}/rollback`) runs the inverse statement:
- update: restore the `before` values
- delete: re-insert `before`
- insert: delete by pk

It runs only if the row still equals the `after` image, else `409 conflict`.
Before/after images are kept with the change for 30 days. After that, the change is
`reversible:false`.

## 5. SQL console

| Method | Path | Class | Sudo | Notes |
|---|---|---|---|---|
| POST | `/api/db/{engine}/query` | read (audited, not a mutation) | – | read-only mode |
| POST | `/api/db/{engine}/query/preview` | D | ✓ | write mode |
| POST | `/api/db/{engine}/query/apply` | D (sync) or J (dangerous statements) | ✓ | write mode |

**Common rules:**
- Body: `{"database":"crm_prod","sql":"…","max_rows":1000}`. `sql` is ≤ 64 KiB.
- **One statement only.** The SQL is parsed with the `sqlparser` crate (MySqlDialect or
  PostgreSqlDialect). More than one statement gives `400 invalid`. A trailing `;` is
  allowed.
- **Row cap.** Rows are streamed. After `min(max_rows, 1000)` rows the agent stops
  reading, kills the statement, and returns `truncated:true`.
- **30 s timeout:**
  - MySQL: `SET SESSION max_execution_time = 30000`. This covers SELECT only.
  - MariaDB: `SET SESSION max_statement_time = 30`. This covers all statements.
  - Postgres: `SET statement_timeout = '30s'`.
  - **Always** also a client-side 30 s timer. On expiry, run `KILL QUERY <connection_id>`
    (MySQL) or `pg_cancel_backend(<pid>)` from an admin connection, and return
    `504 timeout`.
- **Audit.** Every query, read or write, writes to `query_audit`:
  ts, ip, engine, database, mode, `sha256(sql)`, the first 500 chars of SQL, rows
  returned or affected, elapsed ms, result.

  Writes also write a Phase 0 `audit_log` row. Query text is stored **unredacted**
  (Q18).
- **Response** (read, or synchronous write):
  ```json
  {"columns":[{"name","type"}],"rows":[[…]],"row_count":3,"affected":null,"truncated":false,"elapsed_ms":42,"warnings":[]}
  ```

**Read-only mode (default).**
- Connection: the **`nexacloud_ro`** user, with `SET SESSION TRANSACTION READ ONLY`
  (MySQL) or `SET default_transaction_read_only = on` (PG).
- Allowed statement kinds (parsed): `SELECT`, `WITH … SELECT`, `SHOW`,
  `DESCRIBE`/`DESC`, `EXPLAIN`.
- Anything else, or SQL that fails to parse, gives
  `400 {"code":"invalid","error":"this statement writes; switch to write mode"}`.
- The server-side read-only user and transaction are the real guard. The parser only
  produces a better message.

**Write mode.**
- **Preview** classifies the statement:
  ```json
  {"kind":"db_query","plan_hash":"…","statement_kind":"update|delete|insert|replace|ddl|drop|truncate|grant|other",
    "tables":["contacts"],"has_where":false,"dangerous":true,"confirm_required":"contacts","affected_estimate":120400,"backup_first":true,"reversible":false}
  ```
- `affected_estimate`:
  - UPDATE and DELETE on one table: the `rows` sum from `EXPLAIN <statement>`.
  - Otherwise: `null`.
- **Dangerous, needs a typed `confirm`:**

  | Statement | Confirm text |
  |---|---|
  | `DROP DATABASE` / `SCHEMA` | the database name |
  | `DROP TABLE` / `VIEW` / `INDEX` / … | the object name |
  | `TRUNCATE` | the table name |
  | `DELETE` without `WHERE` | the table name |
  | `UPDATE` without `WHERE` | the table name |
  | `ALTER TABLE … DROP …` | the table name |
- **Apply** body: the preview body plus `{"plan_hash","confirm":"contacts"}`. It runs on the
  **admin** connection.
  - Non-dangerous statements run synchronously (class D). DML is wrapped in
    `START TRANSACTION … COMMIT`.
  - Dangerous statements run as a **J**, returning `202 {"change_id","job_id","plan_hash"}`. The
    job takes `db:<engine>/<db>`.
    1. If `backup_first` (the default), dump the affected tables with
       `mysqldump --defaults-extra-file=… --single-transaction <db> <tables…>`, or the
       whole DB for `DROP DATABASE`. Store it as a backup artifact labelled
       `Before SQL console <change_id8>`.
    2. Execute.
  - The job result is `{"affected":n,"backup_point_id":…}`.

## 6. Databases, users and grants (class C: server objects, no files)

The plan's `files` is empty. `commands` lists the SQL with passwords shown as
`'<redacted>'`. Rollback runs inverse SQL.

| Method | Path | Sudo | Confirm | Locks |
|---|---|---|---|---|
| POST | `/api/db/{engine}/databases/preview` / `apply` | – | – | `db-server:{engine}`, `db:<engine>/<name>` (+ `site:<id>` if `connect_project`) |
| POST | `/api/db/{engine}/databases/{db}/drop/preview` / `apply` | ✓ | db name | `db-server:{engine}`, `db:<engine>/<db>` (J: backup → drop) |
| GET | `/api/db/{engine}/users` | – | – | – |
| POST | `/api/db/{engine}/users/preview` / `apply` | – | – | `db-server:{engine}` |
| POST | `/api/db/{engine}/users/{user}/grants/preview` / `apply` | – | – | `db-server:{engine}` |
| POST | `/api/db/{engine}/users/{user}/password/preview` / `apply` | ✓ | – | `db-server:{engine}` (+ `site:<id>`) |
| POST | `/api/db/{engine}/users/{user}/drop/preview` / `apply` | ✓ | user name | `db-server:{engine}` |

**Create database.**
```json
{"name":"blog","collation":null,"user":{"name":"blog","host":"localhost"},"privileges":"all","connect_project":"blog.nexadev.io"}
```
- `name`: `^[a-z0-9_]{1,64}$`. It must not exist, and must not be a system schema or
  start with `nc_verify_`.
- `user` may be `null`. If the user already exists, the database is granted to it.
- `collation` default:

  | Engine | Default collation |
  |---|---|
  | MySQL 8 | `utf8mb4_0900_ai_ci` |
  | MariaDB | `utf8mb4_unicode_ci` |
  | PG | encoding `UTF8`, `TEMPLATE template0` |
- **Password.** The user password (32 chars, `[A-Za-z0-9]`) is generated **at apply**,
  because Phase 0 rebuilds the plan at apply. The preview shows `<generated on apply>`.
  The job `result` returns `{"password":"…"}` once, and the mockup's Copy and New buttons
  act on that result (New = password reset, §6).
- **SQL** (MySQL), on the admin connection:
  ```sql
  CREATE DATABASE `blog` CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci;
  CREATE USER 'blog'@'localhost' IDENTIFIED BY '<pw>';
  GRANT ALL PRIVILEGES ON `blog`.* TO 'blog'@'localhost';
  ```
- **SQL** (PG):
  ```sql
  CREATE ROLE "blog" LOGIN PASSWORD '<pw>';
  CREATE DATABASE "blog" OWNER "blog" ENCODING 'UTF8' TEMPLATE template0;
  ```
- **`connect_project`** adds a `.env` change (projects-deploys.md §9.7) to the same plan.
  It sets `DB_CONNECTION` (`mysql`/`mariadb`/`pgsql`), `DB_HOST=127.0.0.1`,
  `DB_PORT`, `DB_DATABASE`, `DB_USERNAME` and `DB_PASSWORD`, then runs `config:cache`.
- **Rollback** drops the user and the database, but only while the database has 0
  tables, else `409 conflict`.

**Users list.**
```json
[{"name":"shop_user","host":"localhost","databases":["shop_prod"],"preset":"all|read_write|read_only|backup|custom","grants":["GRANT ALL PRIVILEGES ON `shop_prod`.* TO …"],"system":false}]
```
- MySQL: `mysql.user` plus `SHOW GRANTS FOR` each user.
- `system:true` covers `root`, `mysql.*`, `debian-sys-maint` and `nexacloud_*`. These are
  hidden unless `?all=1`.

**Add user / grants.**
- Add user body:
  `{"name":"report","host":"localhost","grants":[{"database":"shop_prod","preset":"read_only"}]}`.
  `host` is one of `localhost`, `127.0.0.1` or `::1`. Remote hosts are **OUT** (they
  conflict with the advisor `db-public-bind` rule).
- Grants body: `{"host":"localhost","set":[{"database":"…","preset":"…"}],"revoke":["db"]}`.

| Preset | Privileges |
|---|---|
| `all` | `ALL PRIVILEGES ON db.*` |
| `read_write` | `SELECT, INSERT, UPDATE, DELETE, CREATE TEMPORARY TABLES, EXECUTE, LOCK TABLES ON db.*` |
| `read_only` | `SELECT, SHOW VIEW ON db.*` |
| `backup` (mockup "Read + lock") | `SELECT, SHOW VIEW, LOCK TABLES, EVENT, TRIGGER, PROCESS ON *.*` (database = `*`) |

PG presets map to:
- `all`: `OWNER` or `GRANT ALL ON DATABASE` + schema `public`
- `read_only`: `pg_read_all_data` for `*`, else `GRANT CONNECT` + `GRANT SELECT ON ALL TABLES IN SCHEMA public` + default privileges
- `backup`: `pg_read_all_data`

**Reset password.** README rule R1 applies.
- Body: `{"host":"localhost","update_project_env":"shop.nexadev.io"|null}`.
- The new password is generated at apply and returned **once** in the job `result`
  (`{"password":"…"}`).
- With `update_project_env`, the same change rewrites that project's `DB_PASSWORD` and
  runs `config:cache`. Without it, the UI warns that the app will lose access.

**Drop database.** Body: `{"confirm":"<db>","backup_first":true}`. It runs as a J:
dump → `DROP DATABASE`. Refused with `409` while a project's `.env` references the
database, unless `"force":true`.

## 7. Slow queries
`GET /api/db/{engine}/slow-queries?range=24h&threshold_ms=1000` (P2 MySQL, P3 PG):
```json
{"source":"performance_schema|slow_log|pg_stat_statements","threshold_ms":1000,"over_threshold":3,
 "items":[{"sql":"select * from `contacts` where `email` like ?","database":"crm_prod","avg_ms":2400,"max_ms":3100,"count":41,"total_ms":98400}]}
```
**MySQL, preferred source: `performance_schema`.**
- Every hour the agent snapshots
  `events_statements_summary_by_digest(SCHEMA_NAME, DIGEST, DIGEST_TEXT, COUNT_STAR, SUM_TIMER_WAIT, MAX_TIMER_WAIT)`
  into SQLite `slow_digest_snap` (kept 8 days).
- The 24 h figures are the current values minus the snapshot nearest to `now − 24h`.
  When `COUNT_STAR` decreased (server restart), the current values are used as-is.
- Items are listed when `avg ≥ threshold` or `max ≥ threshold`, sorted by `total_ms`,
  top 20.
- Timer units are picoseconds; divide by 10⁹ for ms.

**MySQL, fallback: the slow log.** Used when `performance_schema` is OFF. Parse
`/var/log/mysql/mysql-slow.log`:
- `# Query_time:` entries from the last 24 h, reading at most 100 MB from the end.
- Normalise literals to `?` with the tokenizer from `sqlparser`.
- `long_query_time = 1` comes from the advisor baseline (advisor.md §3.1).

**PostgreSQL (P3).** Requires a restart-tier advisor change that writes
`/etc/postgresql/<v>/main/conf.d/zz-nexacloud.conf` with
`shared_preload_libraries = 'pg_stat_statements'` and `pg_stat_statements.track = top`,
then restarts the server and runs `CREATE EXTENSION IF NOT EXISTS pg_stat_statements`
in database `postgres`. The 24 h delta is computed by the same snapshot method from
`calls`, `total_exec_time` and `max_exec_time`. Without the extension the response is
`{"source":null,"items":[],"needs":"pg_stat_statements"}`.

## 8. Export and import

| Method | Path | Class | Sudo | Locks |
|---|---|---|---|---|
| POST | `/api/db/{engine}/databases/{db}/export` | J | – | `db:<engine>/<db>` |
| POST | `/api/db/{engine}/databases/{db}/import/upload` | S | ✓ | – |
| POST | `/api/db/{engine}/databases/{db}/import/preview` / `apply` | C (preview) → J (apply), `reversible:false` | ✓ | `db:<engine>/<db>`, heavy |

**Export.**
- Body: `{"tables":null|["contacts"],"structure_only":false}`.
- It runs the backups.md §4 dump with optional `<tables…>` and `--no-data`.
- It produces a backup artifact labelled `Export`. The result has `point_id`, and the
  file downloads via `POST /api/backup/points/{id}/download`.

**Upload.**
- Raw body (`application/octet-stream`) with an `X-Filename` header matching
  `^[A-Za-z0-9._-]{1,128}\.(sql|sql\.gz|gz|dump|pgdump)$`.
- The size cap is the config `import_max_bytes`, default 2 GiB. Over it: `413`.
- Streamed to `/var/lib/nexacloud/uploads/<uuid>` (0600). It expires after 24 h.
- Returns `{"upload_id","bytes","sha256","format":"sql|sql.gz|pg_custom"}`.
- Behind the panel domain, nginx `client_max_body_size 2G` applies (sites.md §11).

**Preview.** Body `{"upload_id","target":"existing|new","new_name":null,"backup_first":true}`.

The preview scans the stream: all of it for statement-start counts, and the first 64 MiB
for header checks. It returns:
```json
{"kind":"db_import","plan_hash":"…","…":"…",
 "data":{"create_tables":57,"drop_tables":57,"insert_statements":1840,"has_create_database":true,"has_use":true,"definers":3,"gtid_purged":true,"charset":"utf8mb4"},
 "warnings":["CREATE DATABASE/USE statements will be ignored","3 DEFINER clauses will be removed","57 existing tables will be replaced"]}
```

**Apply.**
- Body: the preview body plus `{"plan_hash","confirm":"<db>"}`. `confirm` is required when
  `target: existing`.
- Steps:
  1. With `backup_first`, take a safety dump as a backup artifact.
  2. Stream-decompress and filter each line:
     - drop `CREATE DATABASE` and `USE` statements
     - strip `DEFINER=\`[^\`]*\`@\`[^\`]*\``
     - drop `SET @@GLOBAL.GTID_PURGED`
  3. Pipe the result to
     `mysql --defaults-extra-file=/etc/nexacloud/secrets/mysql-admin.cnf --binary-mode --default-character-set=utf8mb4 <db>`
     on stdin.
     - PG `.sql`: `psql -v ON_ERROR_STOP=1 -X -q -d <db>` with the PGPASSFILE env.
     - PG custom format: `pg_restore --no-owner --role=<owner> -d <db>`.
  4. Verify: the table count is > 0.
  5. Delete the upload.
- **On failure** with `backup_first`, the job loads the safety dump back automatically.
  The result is `{"tables":57,"backup_point_id":…,"restored_after_failure":false}`.

## 9. Connection info
`GET /api/db/connection-info` (P2) feeds the mockup's "Remote access · Local only ·
Copy SSH tunnel settings":
```json
{"mysql":{"host":"127.0.0.1","port":3306,"bound_to":["127.0.0.1"],"remote":false},
 "postgres":{"host":"127.0.0.1","port":5432,"bound_to":["127.0.0.1","::1"],"remote":false},
 "ssh":{"host":"203.0.113.10","port":22},
 "tunnel_text":"SSH host: 203.0.113.10  port: 22  user: <your SSH user>\nDB host: 127.0.0.1  port: 3306\nssh -N -L 3306:127.0.0.1:3306 <user>@203.0.113.10"}
```
Enabling remote DB access is **OUT**.

## 10. Open questions
- **Q18.** The SQL console audit stores query text unredacted, so it may contain
  personal data or secrets typed into queries. Should it store only the hash plus the
  statement kind, or keep 500 chars (current proposal)?
- **Q19.** The PostgreSQL admin role uses `pg_write_all_data` (PG ≥ 14). Ubuntu 22.04
  ships 14, so this is fine. PGDG older versions are not supported for the manager.
