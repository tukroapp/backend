use std::{collections::HashMap, io::Read, iter::repeat_with, path::Path as FsPath, str::FromStr};

use actix_governor::{
    KeyExtractor, SimpleKeyExtractionError,
    governor::{
        NotUntil,
        clock::{Clock, DefaultClock, QuantaInstant},
    },
};

use actix_multipart::{
    MultipartError,
    form::{MultipartFormConfig, tempfile::TempFile, text::Text},
};
use actix_web::{
    HttpMessage, HttpResponse, HttpResponseBuilder,
    dev::ServiceRequest,
    error::InternalError,
    web::{JsonConfig, PathConfig, QueryConfig},
};

use anyhow::Context;
use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::{SaltString, rand_core::OsRng},
};
use derive_more::Display;
use rand::{RngExt, distr::Alphanumeric};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::{ValidationError, ValidationErrors, ValidationErrorsKind};

use crate::{constants::MAX_UPLOAD_SIZE_BYTES, services::auth::SessionData};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiCursorPaginatedResponse<T>
where
    T: Serialize + ToSchema,
{
    pub results: T,
    pub next_cursor: Option<String>,
}

impl<T> ApiCursorPaginatedResponse<T>
where
    T: Serialize + ToSchema,
{
    pub fn new(results: T, next_cursor: Option<String>) -> Self {
        Self {
            results,
            next_cursor,
        }
    }
}

#[derive(Serialize, ToSchema, Debug, Display)]
pub struct ApiResponse<T>
where
    T: Serialize + ToSchema,
{
    #[display("{results}")]
    pub results: T,
}

impl<T> ApiResponse<T>
where
    T: Serialize + ToSchema,
{
    pub fn new(results: T) -> Self {
        Self { results }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ApiErrorNode {
    Errors(Vec<ApiValidationError>),
    Nested(HashMap<String, ApiErrorNode>),
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiValidationError {
    #[schema(example = "required")]
    pub code: String,
    #[schema(example = "O campo 'menu_item_id' é obrigatório")]
    pub message: Option<String>,
    #[schema(example = json!({"value": null}))]
    pub params: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(
    examples(
        json!({
            "error": {
                "email": [{
                    "code": "email_taken",
                    "message": "O e-mail já está cadastrado",
                    "params": { "email": "usuario@example.com" }
                }]
            }
        }),
        json!({
            "error": {
                "items": {
                    "0": {
                        "quantity": [{
                            "code": "required",
                            "message": "O campo 'quantity' é obrigatório",
                            "params": { "value": null }
                        }]
                    }
                }
            }
        }),
    )
)]
pub struct ApiErrorResponse {
    #[schema(value_type = Object)]
    pub error: HashMap<String, ApiErrorNode>,
}

fn convert_validation_errors(errors: &ValidationErrors) -> HashMap<String, ApiErrorNode> {
    let mut map = HashMap::new();

    for (field, kind) in errors.errors() {
        let node = match kind {
            ValidationErrorsKind::Field(field_errors) => {
                let mapped_errors = field_errors.iter().map(ApiValidationError::from).collect();
                ApiErrorNode::Errors(mapped_errors)
            }
            ValidationErrorsKind::Struct(struct_errors) => {
                ApiErrorNode::Nested(convert_validation_errors(struct_errors))
            }
            ValidationErrorsKind::List(list_errors) => {
                let mut list_map = HashMap::new();
                for (index, item_errors) in list_errors {
                    list_map.insert(
                        index.to_string(),
                        ApiErrorNode::Nested(convert_validation_errors(item_errors)),
                    );
                }
                ApiErrorNode::Nested(list_map)
            }
        };
        map.insert(field.to_string(), node);
    }

    map
}

impl From<ValidationErrors> for ApiErrorResponse {
    fn from(errors: ValidationErrors) -> Self {
        Self {
            error: convert_validation_errors(&errors),
        }
    }
}

impl From<&ValidationError> for ApiValidationError {
    fn from(error: &ValidationError) -> Self {
        Self {
            code: error.code.to_string(),
            message: error.message.as_ref().map(|s| s.to_string()),
            // `value` é o que o cliente acabou de mandar: devolver de volta só infla a
            // resposta, e um corpo longo vira um payload de erro enorme.
            params: error
                .params
                .iter()
                .filter(|(key, _)| key.as_ref() != "value")
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }
}

impl ApiErrorResponse {
    pub fn from_single_validation_error(
        field: &str,
        code: &str,
        message: Option<&str>,
        params: Vec<(&str, &str)>,
    ) -> Self {
        let mut error = HashMap::new();

        let parsed_params: HashMap<String, serde_json::Value> = params
            .into_iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect();

        let api_error = ApiValidationError {
            code: code.to_string(),
            message: message.map(|m| m.to_string()),
            params: parsed_params,
        };

        error.insert(field.to_string(), ApiErrorNode::Errors(vec![api_error]));

        Self { error }
    }
}

pub fn invalid_field(
    field: &'static str,
    code: &'static str,
    message: &'static str,
) -> HttpResponse {
    HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
        field,
        code,
        Some(message),
        vec![],
    ))
}

/// Mesmo corpo devolvido pelo middleware `needs_authentication`, para os
/// endpoints que autenticam sozinhos por serem públicos na leitura.
pub fn unauthorized() -> HttpResponse {
    HttpResponse::Unauthorized().json(ApiErrorResponse::from_single_validation_error(
        "authentication",
        "authentication_required",
        Some("Autenticação necessária"),
        vec![],
    ))
}

pub fn parse_multipart_field<T: FromStr>(
    value: Option<Text<String>>,
    field: &'static str,
    code: &'static str,
    message: &'static str,
) -> Result<Option<T>, HttpResponse> {
    value
        .map(|value| {
            value
                .into_inner()
                .parse()
                .map_err(|_| invalid_field(field, code, message))
        })
        .transpose()
}

pub fn parse_nullable_multipart_field<T: FromStr>(
    value: Option<Text<String>>,
    field: &'static str,
    code: &'static str,
    message: &'static str,
) -> Result<Option<Option<T>>, HttpResponse> {
    value
        .map(|value| {
            let value = value.into_inner();
            if value.trim().is_empty() {
                Ok(None)
            } else {
                value
                    .parse()
                    .map(Some)
                    .map_err(|_| invalid_field(field, code, message))
            }
        })
        .transpose()
}

pub fn read_multipart_attachments(
    attachments: Vec<TempFile>,
    field: &'static str,
) -> Result<Vec<(String, Vec<u8>)>, HttpResponse> {
    let mut files = Vec::with_capacity(attachments.len());

    for mut attachment in attachments {
        if attachment.size == 0 {
            return Err(invalid_field(
                field,
                "empty_file",
                "arquivo vazio não é permitido",
            ));
        }
        if attachment.size > MAX_UPLOAD_SIZE_BYTES {
            return Err(invalid_field(
                field,
                "file_too_large",
                "arquivo excede o tamanho máximo permitido",
            ));
        }
        let Some(filename) = attachment.file_name.take() else {
            return Err(invalid_field(
                field,
                "filename_required",
                "nome do arquivo é obrigatório",
            ));
        };
        let Some(filename) = FsPath::new(&filename)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .map(ToString::to_string)
        else {
            return Err(invalid_field(
                field,
                "invalid_filename",
                "nome de arquivo inválido",
            ));
        };

        let mut data = Vec::with_capacity(attachment.size);
        if let Err(error) = attachment.file.read_to_end(&mut data) {
            tracing::error!(
                "Internal server error: erro ao ler arquivo temporário do upload: {error}"
            );
            return Err(HttpResponse::InternalServerError().json(
                ApiErrorResponse::from_single_validation_error(
                    "internal_server_error",
                    "internal_server_error",
                    Some(
                        "Um erro interno do servidor ocorreu, por favor tente novamente mais tarde.",
                    ),
                    vec![],
                ),
            ));
        }
        files.push((filename, data));
    }

    Ok(files)
}

#[derive(Debug, Deserialize, Serialize, IntoParams)]
pub struct CursorPaginationQuery {
    /// Id of the last item of the previous page. Absent on the first page.
    #[param(example = "018f4a65-89ab-7cde-8f01-23456789abce")]
    pub cursor: Option<String>,
    #[param(example = 20)]
    pub limit: Option<i64>,
}

impl CursorPaginationQuery {
    const DEFAULT_LIMIT: i64 = 20;
    const MAX_LIMIT: i64 = 100;

    /// Returns the page size, clamped to the range 1 to 100.
    pub fn limit(&self) -> i64 {
        self.limit
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(1, Self::MAX_LIMIT)
    }

    /// Parses the cursor into a UUID, ignoring a malformed value (treated as first page).
    pub fn cursor_id(&self) -> Option<Uuid> {
        self.cursor.as_deref().and_then(|c| c.parse().ok())
    }
}

/// Fecha a página de uma listagem por cursor: corta o item extra que a consulta
/// trouxe e devolve o cursor da última linha que fica.
///
/// A consulta precisa pedir `limit + 1`. É esse item a mais que prova existir
/// próxima página: uma página com exatamente `limit` itens pode ser o fim da
/// lista, e devolver cursor nesse caso manda o cliente buscar uma página vazia.
pub fn take_page<T, F>(rows: &mut Vec<T>, limit: i64, cursor_of: F) -> Option<String>
where
    F: FnOnce(&T) -> String,
{
    if rows.len() as i64 <= limit {
        return None;
    }

    rows.truncate(limit as usize);
    rows.last().map(cursor_of)
}

pub fn e500<T>(e: T) -> actix_web::Error
where
    T: std::fmt::Debug + std::fmt::Display + 'static,
{
    tracing::error!("Internal server error: {}", e);

    InternalError::from_response(
        e,
        HttpResponse::InternalServerError().json(ApiErrorResponse::from_single_validation_error(
            "internal_server_error",
            "internal_server_error",
            Some("Um erro interno do servidor ocorreu, por favor tente novamente mais tarde."),
            vec![],
        )),
    )
    .into()
}

pub fn db_err(e: sqlx::Error) -> actix_web::Error {
    match &e {
        sqlx::Error::RowNotFound => InternalError::from_response(
            e,
            HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                "not_found",
                "not_found",
                Some("recurso não encontrado"),
                vec![],
            )),
        )
        .into(),
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
            tracing::error!("Database pool unavailable: {}", e);
            InternalError::from_response(
                e,
                HttpResponse::ServiceUnavailable().json(
                    ApiErrorResponse::from_single_validation_error(
                        "service_unavailable",
                        "service_unavailable",
                        Some("serviço temporariamente indisponível, tente novamente"),
                        vec![],
                    ),
                ),
            )
            .into()
        }
        _ => e500(e),
    }
}

pub async fn not_found_service() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "not_found",
        "not_found",
        Some("O recurso solicitado não foi encontrado."),
        vec![],
    ))
}

#[derive(Clone, Debug)]
pub struct UserSessionExtractor;

impl KeyExtractor for UserSessionExtractor {
    type Key = String;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
        req.extensions()
            .get::<SessionData>()
            .map(|session| session.user_id.to_string())
            .ok_or_else(|| {
                SimpleKeyExtractionError::new(
                    "Sessão de usuário não encontrada para extração de chave",
                )
            })
    }

    fn exceed_rate_limit_response(
        &self,
        negative: &NotUntil<QuantaInstant>,
        mut response: HttpResponseBuilder,
    ) -> HttpResponse {
        let wait_time = negative
            .wait_time_from(DefaultClock::default().now())
            .as_secs()
            .to_string();

        response.json(ApiErrorResponse::from_single_validation_error(
            "rate_limit",
            "too_many_requests",
            Some("Limite de requisições excedido."),
            vec![("retry_after_seconds", &wait_time)],
        ))
    }
}

#[derive(Clone, Debug)]
pub struct CustomIpExtractor;

impl KeyExtractor for CustomIpExtractor {
    type Key = std::net::IpAddr;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
        req.peer_addr()
            .map(|socket| socket.ip())
            .ok_or_else(|| SimpleKeyExtractionError::new("IP ausente"))
    }

    fn exceed_rate_limit_response(
        &self,
        negative: &NotUntil<QuantaInstant>,
        mut response: HttpResponseBuilder,
    ) -> HttpResponse {
        let wait_time = negative
            .wait_time_from(DefaultClock::default().now())
            .as_secs()
            .to_string();

        response.json(ApiErrorResponse::from_single_validation_error(
            "rate_limit",
            "too_many_requests",
            Some("Limite de requisições excedido."),
            vec![("retry_after_seconds", &wait_time)],
        ))
    }
}

pub fn json_parse_config() -> JsonConfig {
    JsonConfig::default().error_handler(|err, _req| {
        let msg = err.to_string();
        InternalError::from_response(
            err,
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "invalid_json",
                "invalid_json",
                Some(&msg),
                vec![],
            )),
        )
        .into()
    })
}

pub fn path_parse_config() -> PathConfig {
    PathConfig::default().error_handler(|err, _req| {
        let e = err.to_string();
        InternalError::from_response(
            err,
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "invalid_path_parameter",
                "invalid_path_parameter",
                Some(&e),
                vec![("path_parameter", &e)],
            )),
        )
        .into()
    })
}

pub fn query_parse_config() -> QueryConfig {
    QueryConfig::default().error_handler(|err, _req| {
        let e = err.to_string();
        InternalError::from_response(
            err,
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "invalid_query_parameter",
                "invalid_query_parameter",
                Some(&e),
                vec![],
            )),
        )
        .into()
    })
}

pub fn multipart_parse_config(max_upload_size_bytes: usize) -> MultipartFormConfig {
    MultipartFormConfig::default()
        .total_limit(max_upload_size_bytes)
        .memory_limit(max_upload_size_bytes)
        .error_handler(|err: MultipartError, _req| {
            let msg = err.to_string();
            InternalError::from_response(
                err,
                HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                    "multipart",
                    "invalid_multipart",
                    Some(&msg),
                    vec![],
                )),
            )
            .into()
        })
}

pub fn verify_hash(expected_hash: String, candidate_hash: String) -> Result<(), anyhow::Error> {
    let expected_hash =
        PasswordHash::new(&expected_hash).context("Failed to parse hash in PHC string format.")?;

    Argon2::default().verify_password(candidate_hash.as_bytes(), &expected_hash)?;
    Ok(())
}

pub fn compute_hash(s: String) -> Result<String, anyhow::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(15000, 2, 1, None).unwrap(),
    )
    .hash_password(s.as_bytes(), &salt)?
    .to_string();
    Ok(password_hash)
}

pub fn compute_sha_hash(s: &str) -> String {
    hex::encode(Sha3_256::digest(s.as_bytes()))
}

pub fn gen_random_string(len: usize) -> String {
    let mut rng = rand::rng();
    repeat_with(|| rng.sample(Alphanumeric))
        .map(char::from)
        .take(len)
        .collect::<String>()
}

pub fn deserialize_optional_field<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

pub fn validate_trimmed_string(s: &str) -> Result<(), ValidationError> {
    if s.trim().is_empty() {
        let mut error = ValidationError::new("empty_or_whitespace");
        error.message =
            Some("O valor não pode ser vazio ou conter apenas espaços em branco".into());
        return Err(error);
    }

    Ok(())
}

/// Teto de cada item das listas do perfil (interesses, filmes, etc). O banco só
/// limita a quantidade de itens, o tamanho de cada um é checado aqui.
const MAX_LIST_ITEM_LENGTH: usize = 80;

pub fn validate_list_items(values: &[String]) -> Result<(), ValidationError> {
    if values
        .iter()
        .any(|value| value.chars().count() > MAX_LIST_ITEM_LENGTH)
    {
        let mut error = ValidationError::new("item_too_long");
        error.message = Some("Cada item pode ter no máximo 80 caracteres".into());
        return Err(error);
    }

    Ok(())
}

/// Tags que o editor do frontend escreve. O corpo pode ter marcação, mas marcação
/// sozinha não é mensagem: `[i][/i]` passa no `trim` e chega vazio na tela de quem lê.
const MARKUP_TAGS: [&str; 11] = [
    "b", "i", "u", "s", "c", "quote", "img", "url", "color", "center", "right",
];

/// Teto do que cabe dentro de uma tag, o mesmo do frontend. Colchete com conteúdo
/// maior que isso é texto que a pessoa escreveu, não marcação.
const MAX_TAG_LENGTH: usize = 310;

fn is_markup_tag(inside: &str) -> bool {
    let inside = inside.strip_prefix('/').unwrap_or(inside);
    let name = inside.split_once('=').map_or(inside, |(name, _)| name);

    MARKUP_TAGS.contains(&name.to_ascii_lowercase().as_str())
}

/// O que sobra do corpo depois de tirar as tags, ou seja o que de fato vai ser lido.
fn visible_text(value: &str) -> String {
    let mut visible = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(start) = rest.find('[') {
        visible.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        match after.find(']') {
            Some(end) if end <= MAX_TAG_LENGTH && is_markup_tag(&after[..end]) => {
                rest = &after[end + 1..];
            }
            _ => {
                visible.push('[');
                rest = after;
            }
        }
    }

    visible.push_str(rest);
    visible
}

/// Trecho do corpo sem marcação e com os espaços colapsados, para a prévia de uma
/// mensagem na lista de conversas. Trunca em `max_chars` e acrescenta reticências.
pub fn markup_preview(body: &str, max_chars: usize) -> String {
    let text = visible_text(body);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");

    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }

    let head: String = collapsed.chars().take(max_chars).collect();
    format!("{}...", head.trim_end())
}

/// Mesma regra do `validate_trimmed_string`, mas olhando o texto sem a marcação.
pub fn validate_markup_body(value: &str) -> Result<(), ValidationError> {
    if visible_text(value).trim().is_empty() {
        let mut error = ValidationError::new("empty_or_whitespace");
        error.message = Some("A mensagem não pode ter só formatação, sem texto".into());
        return Err(error);
    }

    Ok(())
}

pub fn validate_decimal_positive_or_zero(value: &Decimal) -> Result<(), ValidationError> {
    if *value < Decimal::ZERO {
        return Err(ValidationError::new("valor deve ser positivo ou zero"));
    }

    if *value >= Decimal::from(1_000_000_000) {
        return Err(ValidationError::new(
            "valor excede o máximo permitido (999999999.9999)",
        ));
    }

    if value.normalize().scale() > 4 {
        return Err(ValidationError::new(
            "valor deve ter no máximo 4 casas decimais",
        ));
    }

    Ok(())
}
