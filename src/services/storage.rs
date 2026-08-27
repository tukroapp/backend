use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::config::{Credentials, RequestChecksumCalculation};
use aws_sdk_s3::primitives::ByteStream;
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

/// As keys carregam UUID e nunca mudam de conteúdo, então o objeto pode ser
/// cacheado para sempre pelo navegador e pelo CDN.
const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

pub const AVATAR_PREFIX: &str = "avatar";

pub const COMMUNITY_AVATAR_PREFIX: &str = "community-avatar";

pub const MEDIA_PREFIX: &str = "media";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Tentativa de acesso indevido (Path Traversal)")]
    PathTraversal,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("Erro no provedor de armazenamento: {0}")]
    Backend(#[from] anyhow::Error),
}

/// Tipo de arquivo reconhecido pelos bytes, nunca pelo nome que o cliente
/// enviou: o bucket é público, e servir HTML a partir dele seria XSS no nosso
/// próprio domínio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileKind {
    pub extension: &'static str,
    pub content_type: &'static str,
}

pub fn sniff_image(data: &[u8]) -> Option<FileKind> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(FileKind {
            extension: "jpg",
            content_type: "image/jpeg",
        })
    } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(FileKind {
            extension: "png",
            content_type: "image/png",
        })
    } else if data.len() > 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some(FileKind {
            extension: "webp",
            content_type: "image/webp",
        })
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some(FileKind {
            extension: "gif",
            content_type: "image/gif",
        })
    } else {
        None
    }
}

pub fn sniff_video(data: &[u8]) -> Option<FileKind> {
    // MP4 e MOV usam o mesmo container ISO BMFF; a marca depois de `ftyp` diz qual é.
    if data.len() > 12 && &data[4..8] == b"ftyp" {
        if &data[8..12] == b"qt  " {
            return Some(FileKind {
                extension: "mov",
                content_type: "video/quicktime",
            });
        }

        return Some(FileKind {
            extension: "mp4",
            content_type: "video/mp4",
        });
    }

    // EBML é o container do WebM e do Matroska. Só o WebM passa, porque é o que
    // o navegador toca sem plugin, e o doctype aparece logo no cabeçalho.
    let header = &data[..data.len().min(64)];

    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
        && header.windows(4).any(|window| window == b"webm")
    {
        return Some(FileKind {
            extension: "webm",
            content_type: "video/webm",
        });
    }

    None
}

#[async_trait]
pub trait StorageService: Send + Sync {
    /// Persiste os bytes e devolve a key, que é o que fica no banco. O host
    /// nunca é persistido: ele muda entre ambientes e é montado na leitura.
    async fn upload(
        &self,
        prefix: &str,
        owner_id: &Uuid,
        data: &[u8],
        file: FileKind,
    ) -> Result<String, StorageError>;

    /// Remove o objeto. Idempotente: key inexistente não é erro.
    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    /// Monta a URL pública de leitura para a key.
    fn public_url(&self, key: &str) -> String;
}

/// Conveniência para os handlers, que sempre lidam com a key opcional do banco.
pub fn public_url_of(storage: &dyn StorageService, key: Option<String>) -> Option<String> {
    key.map(|key| storage.public_url(&key))
}

fn build_key(prefix: &str, owner_id: &Uuid, file: FileKind) -> String {
    format!(
        "{}/{}/{}.{}",
        prefix,
        owner_id,
        Uuid::new_v4(),
        file.extension
    )
}

pub struct LocalStorage {
    pub upload_dir: PathBuf,
    pub base_url: String,
}

impl LocalStorage {
    pub fn new(upload_dir: &str, base_url: &str) -> Self {
        Self {
            upload_dir: PathBuf::from(upload_dir),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl StorageService for LocalStorage {
    async fn upload(
        &self,
        prefix: &str,
        owner_id: &Uuid,
        data: &[u8],
        file: FileKind,
    ) -> Result<String, StorageError> {
        let key = build_key(prefix, owner_id, file);
        let physical_path = self.upload_dir.join(&key);

        fs::create_dir_all(physical_path.parent().unwrap()).await?;
        fs::write(&physical_path, data).await?;

        Ok(key)
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        if key.contains("..") || PathBuf::from(key).is_absolute() {
            return Err(StorageError::PathTraversal);
        }

        match fs::remove_file(self.upload_dir.join(key)).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    fn public_url(&self, key: &str) -> String {
        format!("{}/{}", self.base_url, key)
    }
}

pub struct S3Storage {
    client: aws_sdk_s3::Client,
    bucket: String,
    /// Domínio público de leitura (no R2, o Custom Domain), sem barra no fim.
    public_base_url: String,
}

impl S3Storage {
    pub async fn new(
        endpoint_url: &str,
        access_key_id: &str,
        secret_access_key: &str,
        bucket: &str,
        public_base_url: &str,
    ) -> Self {
        let credentials = Credentials::new(access_key_id, secret_access_key, None, None, "S3");

        let shared = aws_config::defaults(BehaviorVersion::latest())
            .endpoint_url(endpoint_url)
            .credentials_provider(credentials)
            .region(Region::new("auto"))
            .load()
            .await;

        // Alguns provedores S3 nao suportam todos os checksums flexiveis; calcula so quando exigido.
        let conf = aws_sdk_s3::config::Builder::from(&shared)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .build();

        Self {
            client: aws_sdk_s3::Client::from_conf(conf),
            bucket: bucket.to_string(),
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl StorageService for S3Storage {
    async fn upload(
        &self,
        prefix: &str,
        owner_id: &Uuid,
        data: &[u8],
        file: FileKind,
    ) -> Result<String, StorageError> {
        let key = build_key(prefix, owner_id, file);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(data.to_vec()))
            .content_type(file.content_type)
            .cache_control(CACHE_CONTROL)
            .send()
            .await
            .map_err(|e| anyhow::Error::new(e).context("falha ao enviar objeto"))?;

        Ok(key)
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| anyhow::Error::new(e).context("falha ao deletar objeto"))?;

        Ok(())
    }

    fn public_url(&self, key: &str) -> String {
        format!("{}/{}", self.public_base_url, key)
    }
}
