use crate::{
    crypto::decode_master_key,
    error::{CoreError, CoreResult, ErrorCode},
};
use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};
use url::Url;

#[derive(Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub bind: SocketAddr,
    pub public_origin: Url,
    pub storage_dir: PathBuf,
    pub master_key: [u8; 32],
    pub session_ttl: Duration,
    pub report_retention_days: i64,
    pub failed_input_retention_hours: i64,
    pub worker_concurrency: usize,
    pub per_job_chunk_concurrency: usize,
    pub model_concurrency: usize,
    pub job_timeout: Duration,
    pub secure_cookies: bool,
}

impl AppConfig {
    pub fn from_env() -> CoreResult<Self> {
        let database_url = required("TEXTCOMB_DATABASE_URL")?;
        let bind = parse("TEXTCOMB_BIND", "0.0.0.0:8080")?;
        let public_origin = Url::parse(&value("TEXTCOMB_PUBLIC_ORIGIN", "http://localhost:3000"))
            .map_err(|_| {
            CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "TEXTCOMB_PUBLIC_ORIGIN 不是有效 URL",
            )
        })?;
        if !matches!(public_origin.scheme(), "http" | "https")
            || !public_origin.username().is_empty()
            || public_origin.password().is_some()
            || public_origin.path() != "/"
            || public_origin.query().is_some()
            || public_origin.fragment().is_some()
        {
            return Err(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "TEXTCOMB_PUBLIC_ORIGIN 必须是仅包含协议、主机和端口的 http/https 地址",
            ));
        }
        let storage_dir = PathBuf::from(value("TEXTCOMB_STORAGE_DIR", "./data"));
        let master_key = decode_master_key(&required("TEXTCOMB_MASTER_KEY_BASE64")?)?;
        let session_ttl = Duration::from_secs(
            parse::<u64>("TEXTCOMB_SESSION_TTL_HOURS", "24")?
                .checked_mul(3600)
                .ok_or_else(|| {
                    CoreError::public(ErrorCode::ConfigurationInvalid, "会话有效期超出允许范围")
                })?,
        );

        let config = Self {
            database_url,
            bind,
            public_origin,
            storage_dir,
            master_key,
            session_ttl,
            report_retention_days: parse("TEXTCOMB_REPORT_RETENTION_DAYS", "180")?,
            failed_input_retention_hours: parse("TEXTCOMB_FAILED_INPUT_RETENTION_HOURS", "24")?,
            worker_concurrency: parse("TEXTCOMB_WORKER_CONCURRENCY", "10")?,
            per_job_chunk_concurrency: parse("TEXTCOMB_PER_JOB_CHUNK_CONCURRENCY", "2")?,
            model_concurrency: parse("TEXTCOMB_MODEL_CONCURRENCY", "10")?,
            job_timeout: Duration::from_secs(
                parse::<u64>("TEXTCOMB_JOB_TIMEOUT_MINUTES", "120")?
                    .checked_mul(60)
                    .ok_or_else(|| {
                        CoreError::public(
                            ErrorCode::ConfigurationInvalid,
                            "任务超时时间超出允许范围",
                        )
                    })?,
            ),
            secure_cookies: parse("TEXTCOMB_SECURE_COOKIES", "true")?,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> CoreResult<()> {
        if !(1..=100).contains(&self.worker_concurrency)
            || !(1..=16).contains(&self.per_job_chunk_concurrency)
            || !(1..=100).contains(&self.model_concurrency)
        {
            return Err(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "并发配置超出允许范围",
            ));
        }
        if !(1..=3650).contains(&self.report_retention_days)
            || !(1..=168).contains(&self.failed_input_retention_hours)
        {
            return Err(CoreError::public(
                ErrorCode::ConfigurationInvalid,
                "数据保留期限超出允许范围",
            ));
        }
        Ok(())
    }

    pub fn uploads_dir(&self) -> PathBuf {
        self.storage_dir.join("uploads")
    }

    pub fn reports_dir(&self) -> PathBuf {
        self.storage_dir.join("reports")
    }

    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn required(name: &str) -> CoreResult<String> {
    env::var(name).map_err(|_| {
        CoreError::public(
            ErrorCode::ConfigurationInvalid,
            format!("缺少必需环境变量 {name}"),
        )
    })
}

fn parse<T>(name: &str, default: &str) -> CoreResult<T>
where
    T: FromStr,
{
    value(name, default).parse().map_err(|_| {
        CoreError::public(
            ErrorCode::ConfigurationInvalid,
            format!("环境变量 {name} 的值无效"),
        )
    })
}
