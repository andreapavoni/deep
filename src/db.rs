//! SQLite-backed storage for apps, releases, deployments, and addons.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use std::path::Path;
use time::OffsetDateTime;
use ulid::Ulid;

use crate::config::AddonSnapshot;

const MIGRATION_SQL: &str = include_str!("../migrations/001_init.sql");
const MIGRATION_SQL_2: &str = include_str!("../migrations/002_bindings_config.sql");

#[derive(Debug, Clone)]
/// App row stored in SQLite.
pub struct AppRow {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
/// Release row stored in SQLite.
pub struct ReleaseRow {
    pub id: String,
    pub app_id: String,
    pub created_at: String,
    pub git_sha: String,
    pub image_ref: String,
    pub image_digest: String,
    pub config_json: String,
    pub status: String,
}

#[derive(Debug, Clone)]
/// Addon row stored in SQLite.
pub struct AddonRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config_json: String,
    pub created_at: String,
}

/// SQLite storage wrapper with migrations and helpers.
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open or create the database at a path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open sqlite db at {}", path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Start a transaction for multi-step updates.
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        Ok(self.conn.transaction()?)
    }

    /// Create a new app record.
    pub fn create_app(&self, name: &str, repo_path: &str) -> Result<AppRow> {
        let now = now_rfc3339();
        let id = Ulid::new().to_string();
        self.conn.execute(
            "INSERT INTO apps(id, name, repo_path, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![id, name, repo_path, now, now],
        )?;
        Ok(AppRow {
            id,
            name: name.to_string(),
            repo_path: repo_path.to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// List all apps.
    pub fn list_apps(&self) -> Result<Vec<AppRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, repo_path, created_at, updated_at
             FROM apps
             ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AppRow {
                id: row.get(0)?,
                name: row.get(1)?,
                repo_path: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Find an app by name.
    pub fn get_app_by_name(&self, name: &str) -> Result<Option<AppRow>> {
        self.conn
            .query_row(
                "SELECT id, name, repo_path, created_at, updated_at
                 FROM apps WHERE name = ?1",
                params![name],
                |row| {
                    Ok(AppRow {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        repo_path: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("failed to query app")
    }

    /// Remove an app record by name.
    pub fn remove_app(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM apps WHERE name = ?1", params![name])?;
        Ok(())
    }

    /// Insert a release inside a transaction.
    pub fn insert_release(tx: &Transaction<'_>, release: &ReleaseRow) -> Result<()> {
        tx.execute(
            "INSERT INTO releases(id, app_id, created_at, git_sha, image_ref, image_digest, config_json, status)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                release.id,
                release.app_id,
                release.created_at,
                release.git_sha,
                release.image_ref,
                release.image_digest,
                release.config_json,
                release.status
            ],
        )?;
        Ok(())
    }

    /// Update release status.
    pub fn set_release_status(&self, release_id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE releases SET status = ?1 WHERE id = ?2",
            params![status, release_id],
        )?;
        Ok(())
    }

    /// List releases for an app.
    pub fn list_releases(&self, app_id: &str) -> Result<Vec<ReleaseRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, app_id, created_at, git_sha, image_ref, image_digest, config_json, status
             FROM releases
             WHERE app_id = ?1
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![app_id], |row| {
            Ok(ReleaseRow {
                id: row.get(0)?,
                app_id: row.get(1)?,
                created_at: row.get(2)?,
                git_sha: row.get(3)?,
                image_ref: row.get(4)?,
                image_digest: row.get(5)?,
                config_json: row.get(6)?,
                status: row.get(7)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Get a release by id.
    pub fn get_release_by_id(&self, release_id: &str) -> Result<Option<ReleaseRow>> {
        self.conn
            .query_row(
                "SELECT id, app_id, created_at, git_sha, image_ref, image_digest, config_json, status
                 FROM releases
                 WHERE id = ?1",
                params![release_id],
                |row| {
                    Ok(ReleaseRow {
                        id: row.get(0)?,
                        app_id: row.get(1)?,
                        created_at: row.get(2)?,
                        git_sha: row.get(3)?,
                        image_ref: row.get(4)?,
                        image_digest: row.get(5)?,
                        config_json: row.get(6)?,
                        status: row.get(7)?,
                    })
                },
            )
            .optional()
            .context("failed to query release")
    }

    /// Delete a release by id.
    pub fn delete_release(&self, release_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM releases WHERE id = ?1", params![release_id])?;
        Ok(())
    }

    /// Get the current release id for an app.
    pub fn current_release_id(&self, app_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT release_id FROM current_releases WHERE app_id = ?1",
                params![app_id],
                |row| row.get(0),
            )
            .optional()
            .context("failed to query current release")
    }

    /// Set the current release for an app inside a transaction.
    pub fn set_current_release(tx: &Transaction<'_>, app_id: &str, release_id: &str) -> Result<()> {
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO current_releases(app_id, release_id, updated_at)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(app_id) DO UPDATE SET release_id = excluded.release_id, updated_at = excluded.updated_at",
            params![app_id, release_id, now],
        )?;
        Ok(())
    }

    /// Insert a deployment record inside a transaction.
    pub fn insert_deployment(
        tx: &Transaction<'_>,
        deployment_id: &str,
        app_id: &str,
        from_release_id: Option<&str>,
        to_release_id: Option<&str>,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let now = now_rfc3339();
        tx.execute(
            "INSERT INTO deployments(id, app_id, from_release_id, to_release_id, created_at, status, error)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                deployment_id,
                app_id,
                from_release_id,
                to_release_id,
                now,
                status,
                error
            ],
        )?;
        Ok(())
    }

    /// Update a deployment status.
    pub fn update_deployment_status(
        &self,
        deployment_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE deployments SET status = ?1, error = ?2 WHERE id = ?3",
            params![status, error, deployment_id],
        )?;
        Ok(())
    }

    /// Remove deployment rows that reference a release.
    pub fn delete_deployments_for_release(&self, release_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM deployments WHERE from_release_id = ?1 OR to_release_id = ?1",
            params![release_id],
        )?;
        Ok(())
    }

    /// List all addons.
    pub fn list_addons(&self) -> Result<Vec<AddonRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, config_json, created_at
             FROM addons
             ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AddonRow {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                config_json: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Create an addon record.
    pub fn create_addon(&self, name: &str, kind: &str, config_json: &str) -> Result<AddonRow> {
        let now = now_rfc3339();
        let id = Ulid::new().to_string();
        self.conn.execute(
            "INSERT INTO addons(id, name, kind, config_json, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![id, name, kind, config_json, now],
        )?;
        Ok(AddonRow {
            id,
            name: name.to_string(),
            kind: kind.to_string(),
            config_json: config_json.to_string(),
            created_at: now,
        })
    }

    /// Insert or update an addon record by name.
    pub fn upsert_addon(&self, name: &str, kind: &str, config_json: &str) -> Result<AddonRow> {
        if let Some(existing) = self.get_addon_by_name(name)? {
            self.conn.execute(
                "UPDATE addons SET kind = ?1, config_json = ?2 WHERE name = ?3",
                params![kind, config_json, name],
            )?;
            return Ok(AddonRow {
                id: existing.id,
                name: name.to_string(),
                kind: kind.to_string(),
                config_json: config_json.to_string(),
                created_at: existing.created_at,
            });
        }
        self.create_addon(name, kind, config_json)
    }

    /// Delete an addon record by name.
    pub fn destroy_addon(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM addons WHERE name = ?1", params![name])?;
        Ok(())
    }

    /// Bind an addon to an app with optional binding config.
    pub fn bind_addon(&self, app_id: &str, addon_id: &str, config_json: &str) -> Result<()> {
        let now = now_rfc3339();
        let id = Ulid::new().to_string();
        self.conn.execute(
            "INSERT INTO bindings(id, app_id, addon_id, created_at, config_json)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(app_id, addon_id)
             DO UPDATE SET config_json = excluded.config_json",
            params![id, app_id, addon_id, now, config_json],
        )?;
        Ok(())
    }

    /// Unbind an addon from an app.
    pub fn unbind_addon(&self, app_id: &str, addon_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM bindings WHERE app_id = ?1 AND addon_id = ?2",
            params![app_id, addon_id],
        )?;
        Ok(())
    }

    /// Find an addon by name.
    pub fn get_addon_by_name(&self, name: &str) -> Result<Option<AddonRow>> {
        self.conn
            .query_row(
                "SELECT id, name, kind, config_json, created_at
                 FROM addons WHERE name = ?1",
                params![name],
                |row| {
                    Ok(AddonRow {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        kind: row.get(2)?,
                        config_json: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("failed to query addon")
    }

    /// Build addon snapshots for an app, merging binding env overrides.
    pub fn addon_snapshots_for_app(&self, app_id: &str) -> Result<Vec<AddonSnapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT addons.name, addons.kind, addons.config_json, bindings.config_json
             FROM addons
             INNER JOIN bindings ON bindings.addon_id = addons.id
             WHERE bindings.app_id = ?1
             ORDER BY addons.name ASC",
        )?;
        let rows = stmt.query_map(params![app_id], |row| {
            let config_json: String = row.get(2)?;
            let binding_json: String = row.get(3)?;
            let addon_config: Value = serde_json::from_str(&config_json).unwrap_or(Value::Null);
            let binding_config: Value = serde_json::from_str(&binding_json).unwrap_or(Value::Null);
            let config = merge_binding_env(addon_config, binding_config);
            Ok(AddonSnapshot {
                name: row.get(0)?,
                kind: row.get(1)?,
                config,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Insert an event for audit/debug purposes.
    pub fn insert_event(&self, kind: &str, payload_json: &str) -> Result<()> {
        let id = Ulid::new().to_string();
        let ts = now_rfc3339();
        self.conn.execute(
            "INSERT INTO events(id, ts, kind, payload_json) VALUES(?1, ?2, ?3, ?4)",
            params![id, ts, kind, payload_json],
        )?;
        Ok(())
    }

    /// Test the database connection.
    pub fn ping(&self) -> Result<()> {
        self.conn.execute("SELECT 1", [])?;
        Ok(())
    }
}

fn merge_binding_env(addon_config: Value, binding_config: Value) -> Value {
    let mut config = match addon_config {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let binding_env = binding_config
        .as_object()
        .and_then(|map| map.get("env"))
        .and_then(|value| value.as_object());
    if let Some(binding_env) = binding_env {
        let env_entry = config
            .entry("env".to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(env_map) = env_entry {
            for (key, value) in binding_env {
                env_map.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(config)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATION_SQL)?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        conn.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
            params![now_rfc3339()],
        )?;
    }
    let exists: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = 2",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        conn.execute_batch(MIGRATION_SQL_2)?;
        conn.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES(2, ?1)",
            params![now_rfc3339()],
        )?;
    }
    Ok(())
}

fn now_rfc3339() -> String {
    let fmt = time::format_description::well_known::Rfc3339;
    OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
