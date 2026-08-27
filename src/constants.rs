use std::time::Duration;

pub const SESSION_COOKIE_NAME: &str = "sessionid";

pub const SESSION_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

pub const SESSION_REDIS_PREFIX: &str = "tukro_session:";

pub const USER_SESSIONS_REDIS_PREFIX: &str = "user_sessions:";

pub const SESSION_TOKEN_LENGTH: usize = 48;

pub const FROM_EMAIL_ADDRESS: &str = "some@email.com";
pub const EMAIL_BATCH_SIZE: i64 = 10;
pub const EMAIL_MAX_CONCURRENT: usize = 4;
pub const EMAIL_POLL_INTERVAL: Duration = Duration::from_secs(10);

pub const MAX_UPLOAD_SIZE_BYTES: usize = 25 * 1024 * 1024; // 25 MB

pub const MAX_FRIENDS: i64 = 5_000;
pub const MAX_COMMUNITIES_PER_USER: i64 = 1_000;

pub const MAX_PHOTOS_PER_USER: i64 = 10;
pub const MAX_VIDEOS_PER_USER: i64 = 5;
