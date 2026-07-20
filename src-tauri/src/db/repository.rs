use super::models::*;
use sqlx::SqlitePool;

pub struct Repository {
    pool: SqlitePool,
}

impl Repository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // ==================== Channel ====================

    pub async fn get_all_channels(&self) -> Result<Vec<Channel>, sqlx::Error> {
        sqlx::query_as::<_, Channel>("SELECT * FROM channels ORDER BY priority DESC, created_at DESC")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_channel(&self, id: &str) -> Result<Channel, sqlx::Error> {
        sqlx::query_as::<_, Channel>("SELECT * FROM channels WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn get_enabled_channels(&self) -> Result<Vec<Channel>, sqlx::Error> {
        sqlx::query_as::<_, Channel>("SELECT * FROM channels WHERE status = 1 ORDER BY priority DESC, weight DESC")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn create_channel(&self, input: &CreateChannelInput) -> Result<Channel, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        let models = serde_json::to_string(&input.models).unwrap_or_else(|_| "[]".to_string());
        let config = input.config.as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "{}".to_string());
        let model_mapping = input.model_mapping.as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "{}".to_string());

        sqlx::query(
            "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, weight, config, model_mapping, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(&input.name)
        .bind(&input.channel_type)
        .bind(&input.base_url)
        .bind(&input.api_key)
        .bind(&models)
        .bind(input.priority.unwrap_or(0))
        .bind(input.weight.unwrap_or(1))
        .bind(&config)
        .bind(&model_mapping)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        self.get_channel(&id).await
    }

    pub async fn update_channel(&self, input: &UpdateChannelInput) -> Result<Channel, sqlx::Error> {
        let now = now_iso();

        let mut q = sqlx::QueryBuilder::new("UPDATE channels SET updated_at = ");

        q.push_bind(now);

        if let Some(name) = &input.name {
            q.push(", name = ").push_bind(name);
        }
        if let Some(ct) = &input.channel_type {
            q.push(", type = ").push_bind(ct);
        }
        if let Some(base_url) = &input.base_url {
            q.push(", base_url = ").push_bind(base_url);
        }
        if let Some(api_key) = &input.api_key {
            q.push(", api_key = ").push_bind(api_key);
        }
        if let Some(models) = &input.models {
            let m = serde_json::to_string(models).unwrap_or_else(|_| "[]".to_string());
            q.push(", models = ").push_bind(m);
        }
        if let Some(status) = input.status {
            q.push(", status = ").push_bind(status);
        }
        if let Some(priority) = input.priority {
            q.push(", priority = ").push_bind(priority);
        }
        if let Some(weight) = input.weight {
            q.push(", weight = ").push_bind(weight);
        }
        if let Some(config) = &input.config {
            let c = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
            q.push(", config = ").push_bind(c);
        }
        if let Some(mapping) = &input.model_mapping {
            let m = serde_json::to_string(mapping).unwrap_or_else(|_| "{}".to_string());
            q.push(", model_mapping = ").push_bind(m);
        }

        q.push(" WHERE id = ").push_bind(&input.id);
        q.build().execute(&self.pool).await?;

        self.get_channel(&input.id).await
    }

    pub async fn update_channel_status(&self, id: &str, status: i64) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE channels SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_channel(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_channel_test_result(&self, id: &str, ok: bool) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE channels SET last_test_at = ?, last_test_ok = ? WHERE id = ?")
            .bind(&now)
            .bind(if ok { 1 } else { 0 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ==================== API Key ====================

    pub async fn get_all_api_keys(&self) -> Result<Vec<ApiKey>, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_api_key_by_key(&self, key: &str) -> Result<ApiKey, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE key = ? AND status = 1")
            .bind(key)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn create_api_key(&self, input: &CreateApiKeyInput) -> Result<ApiKey, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        let key = format!("sk-waliapi-{}", uuid::Uuid::new_v4().simple());
        let allowed_models = serde_json::to_string(&input.allowed_models.clone().unwrap_or_default()).unwrap_or_else(|_| "[]".to_string());
        let allowed_channels = serde_json::to_string(&input.allowed_channels.clone().unwrap_or_default()).unwrap_or_else(|_| "[]".to_string());

        sqlx::query(
            "INSERT INTO api_keys (id, name, key, status, allowed_models, allowed_channels, quota_limit, quota_used, created_at, updated_at)
             VALUES (?, ?, ?, 1, ?, ?, ?, 0, ?, ?)"
        )
        .bind(&id)
        .bind(&input.name)
        .bind(&key)
        .bind(&allowed_models)
        .bind(&allowed_channels)
        .bind(input.quota_limit.unwrap_or(-1))
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE id = ?")
            .bind(&id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn update_api_key_status(&self, id: &str, status: i64) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE api_keys SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_api_key(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn increment_quota(&self, id: &str, tokens: i64) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE api_keys SET quota_used = quota_used + ? WHERE id = ?")
            .bind(tokens)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ==================== Request Log ====================

    pub async fn create_log(&self, log: &RequestLog) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO request_logs (id, api_key_id, api_key_name, channel_id, channel_name, model, upstream_model, mode, status_code, prompt_tokens, completion_tokens, total_tokens, duration_ms, error_message, is_stream, is_retry, created_at, request_body)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&log.id)
        .bind(&log.api_key_id)
        .bind(&log.api_key_name)
        .bind(&log.channel_id)
        .bind(&log.channel_name)
        .bind(&log.model)
        .bind(&log.upstream_model)
        .bind(&log.mode)
        .bind(log.status_code)
        .bind(log.prompt_tokens)
        .bind(log.completion_tokens)
        .bind(log.total_tokens)
        .bind(log.duration_ms)
        .bind(&log.error_message)
        .bind(log.is_stream)
        .bind(log.is_retry)
        .bind(&log.created_at)
        .bind(&log.request_body)
        .execute(&self.pool)
        .await?;
        // Backfill seq with rowid for new rows
        sqlx::query("UPDATE request_logs SET seq = rowid WHERE id = ? AND seq IS NULL")
            .bind(&log.id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_log(&self, id: &str) -> Result<RequestLog, sqlx::Error> {
        sqlx::query_as::<_, RequestLog>("SELECT * FROM request_logs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn delete_logs_before(&self, before_date: &str) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM request_logs WHERE created_at < ?")
            .bind(before_date)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_all_logs(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM request_logs")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_log(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM request_logs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_logs(&self, limit: i64, offset: i64) -> Result<Vec<RequestLog>, sqlx::Error> {
        sqlx::query_as::<_, RequestLog>(
            "SELECT * FROM request_logs ORDER BY created_at DESC LIMIT ? OFFSET ?"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn search_logs(
        &self,
        keyword: Option<&str>,
        api_key_name: Option<&str>,
        channel_name: Option<&str>,
        model: Option<&str>,
        date_from: Option<&str>,
        date_to: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<RequestLog>, sqlx::Error> {
        let mut q = sqlx::QueryBuilder::new("SELECT * FROM request_logs WHERE 1=1");

        if let Some(kw) = keyword {
            let pattern = format!("%{}%", kw);
            q.push(" AND (api_key_name LIKE ").push_bind(pattern.clone());
            q.push(" OR channel_name LIKE ").push_bind(pattern.clone());
            q.push(" OR model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ").push_bind(pattern.clone());
            q.push(" OR api_key_id LIKE ").push_bind(pattern.clone());
            q.push(" OR id LIKE ").push_bind(pattern);
            q.push(")");
        }

        if let Some(name) = api_key_name {
            let pattern = format!("%{}%", name);
            q.push(" AND api_key_name LIKE ").push_bind(pattern);
        }

        if let Some(name) = channel_name {
            let pattern = format!("%{}%", name);
            q.push(" AND channel_name LIKE ").push_bind(pattern);
        }

        if let Some(m) = model {
            let pattern = format!("%{}%", m);
            q.push(" AND (model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ").push_bind(pattern);
            q.push(")");
        }

        if let Some(from) = date_from {
            q.push(" AND created_at >= ").push_bind(from);
        }

        if let Some(to) = date_to {
            q.push(" AND created_at <= ").push_bind(to);
        }

        q.push(" ORDER BY created_at DESC LIMIT ").push_bind(limit);
        q.push(" OFFSET ").push_bind(offset);

        q.build_query_as::<RequestLog>().fetch_all(&self.pool).await
    }

    pub async fn get_dashboard_stats(&self) -> Result<DashboardStats, sqlx::Error> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let today_prefix = format!("{}%", today);

        let today_requests: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM request_logs WHERE created_at LIKE ?"
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let today_total_tokens: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(total_tokens), 0) FROM request_logs WHERE created_at LIKE ?"
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let active_channels: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channels WHERE status = 1"
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_channels: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_api_keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_tokens: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(total_tokens), 0) FROM request_logs")
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let avg_latency: f64 = sqlx::query_scalar(
            "SELECT COALESCE(AVG(duration_ms), 0) FROM request_logs WHERE created_at LIKE ?"
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0.0);

        Ok(DashboardStats {
            today_requests,
            today_total_tokens,
            active_channels,
            avg_latency_ms: avg_latency,
            total_channels,
            total_api_keys,
            total_requests,
            total_tokens,
        })
    }

    pub async fn get_log_stats(&self, days: i64) -> Result<Vec<LogStats>, sqlx::Error> {
        let since = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(days))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();

        sqlx::query_as::<_, LogStats>(
            "SELECT substr(created_at, 1, 10) as date, COUNT(*) as count, COALESCE(SUM(total_tokens), 0) as total_tokens
             FROM request_logs
             WHERE created_at >= ?
             GROUP BY date
             ORDER BY date DESC"
        )
        .bind(&since)
        .fetch_all(&self.pool)
        .await
    }
}
