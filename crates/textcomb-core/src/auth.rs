use crate::{
    crypto::{hash_password, hash_token, new_session_token, verify_password},
    error::{CoreError, CoreResult, ErrorCode},
};
use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use sqlx::PgPool;
use std::{net::IpAddr, sync::OnceLock, time::Duration};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserIdentity {
    pub id: Uuid,
    pub username: String,
    pub role: String,
}

impl UserIdentity {
    pub fn is_admin(&self) -> bool {
        self.role == "super_admin"
    }
}

pub async fn bootstrap_admin(
    pool: &PgPool,
    username: &str,
    password: &SecretString,
) -> CoreResult<UserIdentity> {
    validate_username(username)?;
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await?;
    if existing != 0 {
        return Err(CoreError::public(
            ErrorCode::Conflict,
            "系统已有用户，不能再次执行首次初始化",
        ));
    }
    create_user(pool, username, password, "super_admin").await
}

pub async fn create_user(
    pool: &PgPool,
    username: &str,
    password: &SecretString,
    role: &str,
) -> CoreResult<UserIdentity> {
    validate_username(username)?;
    if !matches!(role, "super_admin" | "user") {
        return Err(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "用户角色无效",
        ));
    }
    let password_hash = hash_password(password)?;
    let id = Uuid::new_v4();
    let result =
        sqlx::query("INSERT INTO users(id, username, password_hash, role) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(username)
            .bind(password_hash)
            .bind(role)
            .execute(pool)
            .await;
    match result {
        Ok(_) => Ok(UserIdentity {
            id,
            username: username.to_owned(),
            role: role.to_owned(),
        }),
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            Err(CoreError::public(ErrorCode::Conflict, "用户名已存在"))
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn reset_password(
    pool: &PgPool,
    user_id: Uuid,
    password: &SecretString,
) -> CoreResult<()> {
    let password_hash = hash_password(password)?;
    let mut transaction = pool.begin().await?;
    let result =
        sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
            .bind(user_id)
            .bind(password_hash)
            .execute(&mut *transaction)
            .await?;
    if result.rows_affected() != 1 {
        return Err(CoreError::public(ErrorCode::NotFound, "用户不存在"));
    }
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn set_user_enabled(pool: &PgPool, user_id: Uuid, enabled: bool) -> CoreResult<()> {
    let status = if enabled { "active" } else { "disabled" };
    let mut transaction = pool.begin().await?;
    let result = sqlx::query("UPDATE users SET status = $2, updated_at = now() WHERE id = $1")
        .bind(user_id)
        .bind(status)
        .execute(&mut *transaction)
        .await?;
    if result.rows_affected() != 1 {
        return Err(CoreError::public(ErrorCode::NotFound, "用户不存在"));
    }
    if !enabled {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

pub async fn login(
    pool: &PgPool,
    username: &str,
    password: &SecretString,
    ip: Option<IpAddr>,
    user_agent: Option<&str>,
    ttl: Duration,
) -> CoreResult<(UserIdentity, String, OffsetDateTime)> {
    let attempt_username: String = username.chars().take(64).collect();
    if recent_failures(pool, &attempt_username, ip).await? >= 5 {
        return Err(CoreError::public(
            ErrorCode::RateLimited,
            "登录失败次数过多，请 15 分钟后重试",
        ));
    }
    if username.chars().count() > 64 || password.expose_secret().chars().count() > 128 {
        record_attempt(pool, &attempt_username, ip, false).await?;
        return Err(CoreError::public(
            ErrorCode::Unauthorized,
            "用户名或密码错误",
        ));
    }
    #[derive(sqlx::FromRow)]
    struct LoginRow {
        id: Uuid,
        username: String,
        role: String,
        password_hash: String,
        status: String,
    }
    let row = sqlx::query_as::<_, LoginRow>(
        r#"
        SELECT id, username, role, password_hash, status
        FROM users WHERE lower(username) = lower($1)
        "#,
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    let valid = row
        .as_ref()
        .is_some_and(|row| row.status == "active" && verify_password(password, &row.password_hash));
    record_attempt(pool, &attempt_username, ip, valid).await?;
    if !valid {
        return Err(CoreError::public(
            ErrorCode::Unauthorized,
            "用户名或密码错误",
        ));
    }
    let row = row.expect("valid login has a row");
    let (raw_token, token_hash) = new_session_token();
    let session_id = Uuid::new_v4();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::seconds(ttl.as_secs() as i64);
    let user_agent_hash = user_agent.map(hash_token);
    sqlx::query(
        r#"
        INSERT INTO sessions(id, user_id, token_hash, expires_at, user_agent_hash, ip_address)
        VALUES($1,$2,$3,$4,$5,$6::inet)
        "#,
    )
    .bind(session_id)
    .bind(row.id)
    .bind(token_hash)
    .bind(expires_at)
    .bind(user_agent_hash)
    .bind(ip.map(|value| value.to_string()))
    .execute(pool)
    .await?;
    Ok((
        UserIdentity {
            id: row.id,
            username: row.username,
            role: row.role,
        },
        raw_token,
        expires_at,
    ))
}

pub async fn authenticate(pool: &PgPool, raw_token: &str) -> CoreResult<UserIdentity> {
    let token_hash = hash_token(raw_token);
    let user = sqlx::query_as::<_, UserIdentity>(
        r#"
        SELECT users.id, users.username, users.role
        FROM sessions
        JOIN users ON users.id = sessions.user_id
        WHERE sessions.token_hash = $1
          AND sessions.expires_at > now()
          AND users.status = 'active'
        "#,
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CoreError::public(ErrorCode::Unauthorized, "会话无效或已过期"))?;
    sqlx::query("UPDATE sessions SET last_seen_at = now() WHERE token_hash = $1")
        .bind(hash_token(raw_token))
        .execute(pool)
        .await?;
    Ok(user)
}

pub async fn logout(pool: &PgPool, raw_token: &str) -> CoreResult<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(hash_token(raw_token))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn cleanup_sessions(pool: &PgPool) -> CoreResult<u64> {
    let sessions = sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
        .execute(pool)
        .await?
        .rows_affected();
    let attempts =
        sqlx::query("DELETE FROM login_attempts WHERE attempted_at < now() - interval '24 hours'")
            .execute(pool)
            .await?
            .rows_affected();
    Ok(sessions + attempts)
}

fn validate_username(username: &str) -> CoreResult<()> {
    static USERNAME: OnceLock<Regex> = OnceLock::new();
    let regex = USERNAME.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.-]{2,63}$").expect("username regex is valid")
    });
    if !regex.is_match(username) {
        return Err(CoreError::public(
            ErrorCode::ConfigurationInvalid,
            "用户名必须为 3–64 位字母、数字、点、下划线或连字符",
        ));
    }
    Ok(())
}

async fn recent_failures(pool: &PgPool, username: &str, ip: Option<IpAddr>) -> CoreResult<i64> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT count(*) FROM login_attempts
        WHERE NOT success
          AND attempted_at > now() - interval '15 minutes'
          AND (
              lower(username) = lower($1)
              OR ($2::inet IS NOT NULL AND ip_address = $2::inet)
          )
        "#,
    )
    .bind(username)
    .bind(ip.map(|value| value.to_string()))
    .fetch_one(pool)
    .await?)
}

async fn record_attempt(
    pool: &PgPool,
    username: &str,
    ip: Option<IpAddr>,
    success: bool,
) -> CoreResult<()> {
    sqlx::query("INSERT INTO login_attempts(username, ip_address, success) VALUES($1,$2::inet,$3)")
        .bind(username)
        .bind(ip.map(|value| value.to_string()))
        .bind(success)
        .execute(pool)
        .await?;
    Ok(())
}
