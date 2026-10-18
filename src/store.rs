use crate::{
    config::{Config, now},
    error::{Error, Result},
};
use serde_json::{Value, json};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{str::FromStr, sync::Arc};
#[derive(Clone)]
pub struct Store {
    pub pool: SqlitePool,
}
impl Store {
    pub async fn open(path: &std::path::Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            tokio::fs::create_dir_all(p)
                .await
                .map_err(Error::internal)?;
        }
        #[cfg(unix)]
        {
            let mut file = tokio::fs::OpenOptions::new();
            file.create(true).write(true).mode(0o600);
            file.open(path).await.map_err(Error::internal)?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(Error::internal)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(Error::internal)?;
        for statement in [
            "CREATE TABLE IF NOT EXISTS quotas(tenant TEXT NOT NULL,day INTEGER NOT NULL,spent REAL NOT NULL DEFAULT 0,reserved REAL NOT NULL DEFAULT 0,active INTEGER NOT NULL DEFAULT 0,minute INTEGER NOT NULL DEFAULT 0,requests INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(tenant,day))",
            "CREATE TABLE IF NOT EXISTS requests(id TEXT PRIMARY KEY,tenant TEXT NOT NULL,day INTEGER NOT NULL,reserved REAL NOT NULL,cost REAL,started INTEGER NOT NULL,status TEXT NOT NULL,metadata TEXT)",
            "CREATE INDEX IF NOT EXISTS requests_tenant ON requests(tenant,started)",
            "CREATE TABLE IF NOT EXISTS sessions(tenant TEXT NOT NULL,id TEXT NOT NULL,model TEXT NOT NULL,effort TEXT NOT NULL,updated INTEGER NOT NULL,PRIMARY KEY(tenant,id))",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .map_err(Error::internal)?;
        }
        Ok(Self { pool })
    }
    pub async fn recover(&self) -> Result<()> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query("UPDATE quotas SET spent=spent+reserved,reserved=0,active=0")
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query("UPDATE requests SET cost=reserved,status='interrupted_estimate' WHERE status='pending'").execute(&mut *tx).await.map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }
    pub async fn reserve(
        self: &Arc<Self>,
        tenant: &str,
        amount: f64,
        cfg: &Config,
    ) -> Result<Reservation> {
        if !amount.is_finite() || amount < 0.0 || amount > cfg.max_request_estimated_usd {
            return Err(Error::new(
                402,
                "estimated_budget_exceeded",
                "Request exceeds configured estimated cost limit",
            ));
        }
        let day = now() / 86400;
        let minute = now() / 60;
        let id = uuid::Uuid::new_v4().to_string();
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query("INSERT INTO quotas(tenant,day) VALUES(?,?) ON CONFLICT DO NOTHING")
            .bind(tenant)
            .bind(day)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let changed=sqlx::query("UPDATE quotas SET reserved=reserved+?,active=active+1,requests=CASE WHEN minute=? THEN requests+1 ELSE 1 END,minute=? WHERE tenant=? AND day=? AND (SELECT COALESCE(SUM(q.active),0) FROM quotas q WHERE q.tenant=quotas.tenant)<? AND spent+reserved+?<=? AND (minute<>? OR requests<?)").bind(amount).bind(minute).bind(minute).bind(tenant).bind(day).bind(cfg.tenant_concurrent).bind(amount).bind(cfg.daily_estimated_budget_usd).bind(minute).bind(cfg.requests_per_minute).execute(&mut *tx).await.map_err(Error::internal)?.rows_affected();
        if changed == 0 {
            return Err(Error::new(
                429,
                "tenant_limit_reached",
                "Tenant concurrency, rate or estimated daily budget limit reached",
            ));
        }
        sqlx::query("INSERT INTO requests(id,tenant,day,reserved,started,status)VALUES(?,?,?,?,?,'pending')").bind(&id).bind(tenant).bind(day).bind(amount).bind(now()).execute(&mut *tx).await.map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Reservation {
            id,
            tenant: tenant.into(),
            reserved: amount,
            store: self.clone(),
            finished: false,
        })
    }
    pub async fn settle(&self, id: &str, cost: f64, status: &str, metadata: &Value) -> Result<()> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let row =
            sqlx::query("SELECT tenant,day,reserved FROM requests WHERE id=? AND status='pending'")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if let Some(r) = row {
            let tenant: String = r.get(0);
            let day: i64 = r.get(1);
            let reserved: f64 = r.get(2);
            let cost = if cost.is_finite() && cost >= 0.0 {
                cost
            } else {
                reserved
            };
            sqlx::query("UPDATE quotas SET reserved=MAX(0,reserved-?),active=MAX(0,active-1),spent=spent+? WHERE tenant=? AND day=?").bind(reserved).bind(cost).bind(tenant).bind(day).execute(&mut *tx).await.map_err(Error::internal)?;
            sqlx::query("UPDATE requests SET cost=?,status=?,metadata=? WHERE id=?")
                .bind(cost)
                .bind(status)
                .bind(metadata.to_string())
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }
    pub async fn usage(&self, tenant: &str) -> Result<Value> {
        let active = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(active),0) FROM quotas WHERE tenant=?",
        )
        .bind(tenant)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::internal)?;
        let row = sqlx::query("SELECT spent,reserved,active FROM quotas WHERE tenant=? AND day=?")
            .bind(tenant)
            .bind(now() / 86400)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::internal)?;
        Ok(if let Some(r) = row {
            json!({"spent_or_conservative_estimate_usd":r.get::<f64,_>(0),"reserved_usd":r.get::<f64,_>(1),"active":active})
        } else {
            json!({"spent_or_conservative_estimate_usd":0,"reserved_usd":0,"active":active})
        })
    }
    pub async fn request(&self, tenant: &str, id: &str) -> Result<Value> {
        let r = sqlx::query(
            "SELECT id,status,cost,reserved,metadata FROM requests WHERE tenant=? AND id=?",
        )
        .bind(tenant)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::new(404, "not_found", "Request not found"))?;
        let m: Option<String> = r.get(4);
        Ok(
            json!({"id":r.get::<String,_>(0),"status":r.get::<String,_>(1),"cost_usd":r.get::<Option<f64>,_>(2),"reserved_usd":r.get::<f64,_>(3),"routing":m.and_then(|s|serde_json::from_str::<Value>(&s).ok())}),
        )
    }
    pub async fn session(
        &self,
        tenant: &str,
        id: &str,
        ttl: i64,
    ) -> Result<Option<(String, String)>> {
        let r =
            sqlx::query("SELECT model,effort FROM sessions WHERE tenant=? AND id=? AND updated>?")
                .bind(tenant)
                .bind(id)
                .bind(now() - ttl)
                .fetch_optional(&self.pool)
                .await
                .map_err(Error::internal)?;
        Ok(r.map(|r| (r.get(0), r.get(1))))
    }
    pub async fn bind_session(
        &self,
        tenant: &str,
        id: &str,
        model: &str,
        effort: &str,
    ) -> Result<()> {
        sqlx::query("INSERT INTO sessions(tenant,id,model,effort,updated)VALUES(?,?,?,?,?) ON CONFLICT(tenant,id) DO UPDATE SET model=excluded.model,effort=excluded.effort,updated=excluded.updated").bind(tenant).bind(id).bind(model).bind(effort).bind(now()).execute(&self.pool).await.map_err(Error::internal)?;
        Ok(())
    }
    pub async fn prune(&self) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE updated<?")
            .bind(now() - 86400 * 7)
            .execute(&self.pool)
            .await
            .map_err(Error::internal)?;
        sqlx::query("DELETE FROM requests WHERE started<? AND status<>'pending'")
            .bind(now() - 86400 * 30)
            .execute(&self.pool)
            .await
            .map_err(Error::internal)?;
        sqlx::query("DELETE FROM quotas WHERE day<? AND active=0")
            .bind(now() / 86400 - 30)
            .execute(&self.pool)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }
}
pub struct Reservation {
    pub id: String,
    pub tenant: String,
    pub reserved: f64,
    store: Arc<Store>,
    finished: bool,
}
impl Reservation {
    pub async fn finish(mut self, cost: f64, status: &str, metadata: &Value) -> Result<()> {
        self.store.settle(&self.id, cost, status, metadata).await?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.finished {
            let store = self.store.clone();
            let id = self.id.clone();
            let amount = self.reserved;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move{let _=store.settle(&id,amount,"cancelled_estimate",&json!({"billing":"conservative reservation; provider cost unavailable"})).await;});
            }
        }
    }
}
