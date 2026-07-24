use crate::CoreError;
use crate::error::{CoreResult, ErrorCode};
use std::path::{Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub async fn new(root: impl Into<PathBuf>) -> CoreResult<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("uploads")).await?;
        fs::create_dir_all(root.join("reports")).await?;
        fs::create_dir_all(root.join("tmp")).await?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn write_upload(
        &self,
        document_id: Uuid,
        extension: &str,
        bytes: &[u8],
    ) -> CoreResult<PathBuf> {
        let extension = match extension {
            "txt" | "docx" | "pdf" => extension,
            _ => {
                return Err(CoreError::public(
                    ErrorCode::UnsupportedFormat,
                    "不支持的文件扩展名",
                ));
            }
        };
        let path = self
            .root
            .join("uploads")
            .join(format!("{document_id}.{extension}"));
        fs::write(&path, bytes).await?;
        Ok(path)
    }

    pub async fn read(&self, path: &Path) -> CoreResult<Vec<u8>> {
        ensure_under_root(&self.root, path)?;
        Ok(fs::read(path).await?)
    }

    pub async fn remove(&self, path: &Path) -> CoreResult<()> {
        ensure_under_root(&self.root, path)?;
        match fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn report_pdf_path(&self, report_id: Uuid) -> PathBuf {
        self.root.join("reports").join(format!("{report_id}.pdf"))
    }

    pub fn temporary_path(&self, id: Uuid, extension: &str) -> PathBuf {
        self.root.join("tmp").join(format!("{id}.{extension}"))
    }
}

fn ensure_under_root(root: &Path, path: &Path) -> CoreResult<()> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = if path.exists() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    if !candidate.starts_with(root) {
        return Err(CoreError::public(
            ErrorCode::Forbidden,
            "拒绝访问存储目录以外的路径",
        ));
    }
    Ok(())
}
