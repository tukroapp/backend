use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    routes::profile::slug::{fetch_target, profile_not_found},
    services::auth::SessionData,
    utils::{ApiErrorResponse, ApiResponse, db_err, invalid_field, unauthorized},
};

/// Quem votou zero num eixo fica de fora da conta dele, então `average` é `null`
/// quando ninguém opinou.
#[derive(Serialize, ToSchema)]
pub struct RatingAxis {
    pub average: Option<f64>,
    pub count: i64,
}

#[derive(Serialize, ToSchema)]
pub struct RatingSummary {
    pub trusty: RatingAxis,
    pub cool: RatingAxis,
    pub sexy: RatingAxis,
}

#[derive(Serialize, ToSchema)]
pub struct MyRating {
    pub trusty: i16,
    pub cool: i16,
    pub sexy: i16,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct RatingRequest {
    #[validate(range(min = 0, max = 3))]
    pub trusty: i16,
    #[validate(range(min = 0, max = 3))]
    pub cool: i16,
    #[validate(range(min = 0, max = 3))]
    pub sexy: i16,
}

fn rating_forbidden() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "rating",
        "not_friend",
        Some("Só amigos podem avaliar esse perfil."),
        vec![],
    ))
}

/// Avaliação de quem está suspenso ou desativado sai da média, igual às outras
/// contagens do perfil.
pub async fn fetch_summary(
    pool: &PgPool,
    rated_id: Uuid,
) -> Result<RatingSummary, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE pr.trusty > 0) AS "trusty_count!",
            COUNT(*) FILTER (WHERE pr.cool > 0) AS "cool_count!",
            COUNT(*) FILTER (WHERE pr.sexy > 0) AS "sexy_count!",
            AVG(pr.trusty) FILTER (WHERE pr.trusty > 0)::FLOAT8 AS trusty_average,
            AVG(pr.cool) FILTER (WHERE pr.cool > 0)::FLOAT8 AS cool_average,
            AVG(pr.sexy) FILTER (WHERE pr.sexy > 0)::FLOAT8 AS sexy_average
        FROM profile_rating pr
        INNER JOIN auth_user au ON au.id = pr.rater_id AND au.status = 'active'
        WHERE pr.rated_id = $1
        "#,
        rated_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    Ok(RatingSummary {
        trusty: RatingAxis {
            average: row.trusty_average,
            count: row.trusty_count,
        },
        cool: RatingAxis {
            average: row.cool_average,
            count: row.cool_count,
        },
        sexy: RatingAxis {
            average: row.sexy_average,
            count: row.sexy_count,
        },
    })
}

pub async fn fetch_my_rating(
    pool: &PgPool,
    rater_id: Uuid,
    rated_id: Uuid,
) -> Result<Option<MyRating>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT trusty, cool, sexy
        FROM profile_rating
        WHERE rater_id = $1 AND rated_id = $2
        "#,
        rater_id,
        rated_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| MyRating {
        trusty: row.trusty,
        cool: row.cool,
        sexy: row.sexy,
    }))
}

/// Avaliação só vale entre amigos, então ela morre junto da amizade, seja por
/// desfazer ou por bloqueio. Sem isso daria para avaliar, desfazer a amizade, e
/// deixar a nota lá para sempre.
pub async fn delete_ratings_between(
    tx: &mut Transaction<'static, Postgres>,
    one: Uuid,
    other: Uuid,
) -> Result<(), actix_web::Error> {
    sqlx::query!(
        r#"
        DELETE FROM profile_rating
        WHERE (rater_id = $1 AND rated_id = $2)
           OR (rater_id = $2 AND rated_id = $1)
        "#,
        one,
        other,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    Ok(())
}

#[utoipa::path(
    put,
    path = "/api/v1/profile/{slug}/rating",
    description = "Cria ou substitui a minha avaliação desse perfil. Só amigos avaliam, e cada um avalia o outro uma única vez",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    request_body = RatingRequest,
    responses(
        (status = 200, description = "Avaliação registrada, com o agregado atualizado", body = ApiResponse<RatingSummary>),
        (status = 400, description = "Corpo inválido ou avaliação de si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não são amigos", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Avalia um perfil", skip_all)]
pub async fn put(
    slug: Path<String>,
    payload: Json<RatingRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    if payload.trusty + payload.cool + payload.sexy == 0 {
        return Ok(invalid_field(
            "rating",
            "empty_rating",
            "Dê pelo menos uma nota para avaliar o perfil.",
        ));
    }

    let Some(target) =
        fetch_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(profile_not_found());
    };

    if target.user_id == session.user_id {
        return Ok(invalid_field(
            "rating",
            "self_rating",
            "Você não pode avaliar a si mesmo.",
        ));
    }

    if !target.can_rate {
        return Ok(rating_forbidden());
    }

    sqlx::query!(
        r#"
        INSERT INTO profile_rating (rater_id, rated_id, trusty, cool, sexy)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (rater_id, rated_id) DO UPDATE
        SET trusty = EXCLUDED.trusty,
            cool = EXCLUDED.cool,
            sexy = EXCLUDED.sexy
        "#,
        session.user_id,
        target.user_id,
        payload.trusty,
        payload.cool,
        payload.sexy,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    let summary = fetch_summary(pool.get_ref(), target.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(summary)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/profile/{slug}/rating",
    description = "Remove a minha avaliação desse perfil",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    responses(
        (status = 200, description = "Avaliação removida, com o agregado atualizado", body = ApiResponse<RatingSummary>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou sem avaliação sua", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Remove a avaliação de um perfil", skip_all)]
pub async fn delete(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(profile_not_found());
    };

    let removed = sqlx::query!(
        r#"
        DELETE FROM profile_rating
        WHERE rater_id = $1 AND rated_id = $2
        "#,
        session.user_id,
        target.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if removed == 0 {
        return Ok(
            HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                "rating",
                "not_rated",
                Some("Você ainda não avaliou esse perfil."),
                vec![],
            )),
        );
    }

    let summary = fetch_summary(pool.get_ref(), target.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(summary)))
}
