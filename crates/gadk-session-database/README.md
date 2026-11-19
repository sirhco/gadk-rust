# gadk-session-database

A `SessionService` (from `gadk-session`) that stores sessions, events and app/user state in a
relational database through sqlx: sqlite (feature `sqlite`, on by default) or PostgreSQL
(feature `postgres`). The tables, columns and rows are the ones ADK's database session service
uses, so gadk and ADK services can share one database.

With the facade, enable the `database` feature of `gadk` and use `gadk::session_database`. For
PostgreSQL, also enable this crate's `postgres` feature in your own manifest.

## Example

```rust
// Inside an async fn returning Result.
use gadk_session_database::DatabaseSessionService;

let sessions = DatabaseSessionService::connect("sqlite://sessions.db").await?;
// The service never creates or alters tables on its own: migrate on every startup.
sessions.auto_migrate().await?;
// Pass `Arc::new(sessions)` as `RunnerConfig::session_service`.
```

## Connecting

- `connect(url)` accepts `sqlite:` URLs (the file is created when missing; `sqlite::memory:`
  keeps its single connection open for the pool's lifetime) and `postgres:` / `postgresql:`
  URLs. A scheme whose feature is off is an error naming the feature.
- `from_sqlite_pool` / `from_pg_pool` wrap a pool you built yourself; the crate re-exports
  `sqlx` so the versions match.
- `with_platform` sets the clock and ID source.

## Migrations

`auto_migrate` creates missing tables and adds missing columns; it never alters or drops
existing ones and is safe to run repeatedly. A later release of this crate may add columns, and
writes fail until they exist, so run it on every startup.

## Notes

On sqlite, foreign keys are not enforced on connections opened by `connect`, so deleting a
session leaves its event rows behind; on PostgreSQL the foreign key's `ON DELETE CASCADE`
removes them.
