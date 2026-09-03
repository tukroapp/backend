use std::time::Duration;

pub const SESSION_COOKIE_NAME: &str = "sessionid";

pub const SESSION_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

pub const SESSION_REDIS_PREFIX: &str = "tukro_session:";

pub const USER_SESSIONS_REDIS_PREFIX: &str = "user_sessions:";

pub const SESSION_TOKEN_LENGTH: usize = 48;

/// Tamanho mínimo da senha, valendo no cadastro, na troca, no reset e no
/// `createsuperuser` do CLI.
pub const PASSWORD_MIN_LENGTH: u64 = 8;

/// Data de publicação dos termos e da política de privacidade em vigor. Subir o
/// texto sem subir isso aqui deixa todo mundo aceito numa versão que não existe
/// mais, então as duas coisas andam juntas.
pub const CURRENT_TERMS_VERSION: &str = "2026-09-01";

pub const EMAIL_BATCH_SIZE: i64 = 10;
pub const EMAIL_MAX_CONCURRENT: usize = 4;
pub const EMAIL_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Usado quando a requisição não manda o fuso. O público é brasileiro, então cair
/// em UTC daria um "hoje" errado por três horas todo dia.
pub const DEFAULT_TIME_ZONE: &str = "America/Sao_Paulo";

pub const MAX_UPLOAD_SIZE_BYTES: usize = 25 * 1024 * 1024; // 25 MB

pub const MAX_FRIENDS: i64 = 5_000;
pub const MAX_COMMUNITIES_PER_USER: i64 = 1_000;

// Os tetos de mídia são apertados de propósito: o bucket é o plano gratuito do
// R2, e vídeo de 25 MB é o que estoura os 10 GB primeiro.
pub const MAX_PHOTOS_PER_USER: i64 = 30;
pub const MAX_VIDEOS_PER_USER: i64 = 2;
pub const MAX_ALBUMS_PER_USER: i64 = 5;
pub const MAX_PHOTOS_PER_ALBUM: i64 = 20;
