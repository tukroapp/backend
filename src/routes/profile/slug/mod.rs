pub mod communities;
pub mod fans;
pub mod friends;
pub mod media;
pub mod messages;
pub mod rating;
pub mod scraps;
pub mod testimonials;

use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::{
        me::{RelationshipStatus, Visibility},
        profile::{
            ProfileSummary,
            slug::rating::{MyRating, RatingSummary},
        },
    },
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
        visit::record_visit,
    },
    startup::RedisPool,
    utils::{ApiErrorResponse, ApiResponse, db_err},
};

/// Quantidade de amigos exibida no próprio perfil, antes de abrir a lista completa.
const FRIENDS_PREVIEW_LIMIT: i64 = 9;

/// Perfil detalhado, no estilo das abas social, profissional e pessoal do Orkut.
/// Só chega para quem a privacidade do dono deixa ver.
#[derive(Serialize, ToSchema)]
pub struct ProfileDetails {
    pub relationship_status: Option<RelationshipStatus>,
    pub country: Option<String>,
    pub state: Option<String>,
    pub languages: Vec<String>,
    pub interests: Vec<String>,
    pub who_i_want_to_meet: Option<String>,
    pub occupation: Option<String>,
    pub company: Option<String>,
    pub education: Option<String>,
    pub music: Vec<String>,
    pub movies: Vec<String>,
    pub books: Vec<String>,
    pub tv_shows: Vec<String>,
    pub foods: Vec<String>,
    pub sports: Vec<String>,
    pub pets: Option<String>,
    pub fashion_style: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ProfileResponse {
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
    pub bio: Option<String>,
    pub city: Option<String>,
    pub birthdate: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    /// `null` quando a privacidade do dono esconde o perfil detalhado de quem
    /// está olhando, a mesma regra da lista de amigos.
    pub details: Option<ProfileDetails>,
    /// Relação de quem está olhando com esse perfil: `self`, `anonymous`,
    /// `friends`, `request_sent`, `request_received` ou `none`.
    #[schema(example = "none")]
    pub relationship: String,
    /// `false` quando a privacidade do dono esconde os amigos de quem está
    /// olhando. Nesse caso `friends` vem vazio por falta de permissão, não por
    /// falta de amigos.
    pub friends_visible: bool,
    /// Total de amigos do perfil, `null` quando `friends_visible` é `false`.
    pub friends_count: Option<i64>,
    pub friends: Vec<ProfileSummary>,
    /// `false` quando a privacidade do dono esconde o mural de recados de quem
    /// está olhando.
    pub scraps_visible: bool,
    /// `false` quando quem está olhando não pode deixar recados nesse perfil.
    pub can_write_scrap: bool,
    /// Total de recados no mural, sem contar as respostas. `null` quando
    /// `scraps_visible` é `false`.
    pub scraps_count: Option<i64>,
    /// `false` quando quem está olhando não pode escrever um depoimento para
    /// esse perfil. Sempre `false` no próprio perfil.
    pub can_write_testimonial: bool,
    /// `false` quando a privacidade do dono esconde os depoimentos de quem está
    /// olhando (mesma regra da lista de amigos).
    pub testimonials_visible: bool,
    /// Total de depoimentos aceitos. `null` quando `testimonials_visible` é
    /// `false`. Pendentes e recusados só aparecem para o dono, na moderação.
    pub testimonials_count: Option<i64>,
    /// `false` quando quem está olhando não pode avaliar esse perfil. Só amigos
    /// avaliam, e ninguém avalia a si mesmo.
    pub can_rate: bool,
    pub rating: RatingSummary,
    /// A avaliação de quem está olhando, para preencher o formulário. `null`
    /// para anônimo, para o dono do perfil e para quem ainda não avaliou.
    pub my_rating: Option<MyRating>,
    /// Total de fãs do perfil.
    pub fans_count: i64,
    /// `true` quando quem está olhando já é fã desse perfil.
    pub is_fan: bool,
    /// `false` para anônimo e para o próprio perfil.
    pub can_be_fan: bool,
}

/// Perfil resolvido pelo slug junto das regras de privacidade já avaliadas para
/// quem está olhando (`viewer` é `None` quando a requisição é anônima).
pub struct ProfileTarget {
    pub user_id: Uuid,
    pub can_view_profile_details: bool,
    pub can_read_scrap: bool,
    pub can_write_scrap: bool,
    pub can_write_testimonial: bool,
    pub can_send_message: bool,
    pub can_rate: bool,
    pub relationship: String,
    /// `true` quando quem está olhando já é fã desse perfil.
    pub viewer_is_fan: bool,
    /// Preferência do dono do perfil.
    pub show_profile_visits: bool,
    /// Preferência de quem está olhando. A visita só é registrada quando as duas
    /// pontas mantêm o histórico ligado.
    pub viewer_shows_visits: bool,
}

/// Resolve uma preferência de privacidade para quem está olhando. O dono do
/// perfil sempre passa, inclusive em `nobody`.
fn allows(visibility: Visibility, is_self: bool, is_friend: bool) -> bool {
    match visibility {
        Visibility::Everyone => true,
        Visibility::Friends => is_self || is_friend,
        Visibility::Nobody => is_self,
    }
}

/// Retorna `None` quando o slug não existe, a conta está inativa ou existe
/// bloqueio entre as duas pontas: em todos os casos a resposta é 404.
pub async fn fetch_target(
    pool: &PgPool,
    slug: &str,
    viewer: Option<Uuid>,
) -> Result<Option<ProfileTarget>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            up.user_id,
            up.profile_visibility AS "profile_visibility: Visibility",
            up.scrap_read AS "scrap_read: Visibility",
            up.scrap_write AS "scrap_write: Visibility",
            up.testimonial_write AS "testimonial_write: Visibility",
            up.message_receive AS "message_receive: Visibility",
            up.show_profile_visits,
            COALESCE((
                SELECT viewer.show_profile_visits
                FROM user_profile viewer
                WHERE viewer.user_id = $2::uuid
            ), TRUE) AS "viewer_shows_visits!",
            EXISTS (
                SELECT 1 FROM friend f
                WHERE f.user_id = up.user_id AND f.friend_id = $2::uuid
            ) AS "is_friend!",
            EXISTS (
                SELECT 1 FROM fan
                WHERE fan.idol_id = up.user_id AND fan.fan_id = $2::uuid
            ) AS "viewer_is_fan!",
            EXISTS (
                SELECT 1 FROM block b
                WHERE (b.blocker_id = up.user_id AND b.blocked_id = $2::uuid)
                   OR (b.blocker_id = $2::uuid AND b.blocked_id = up.user_id)
            ) AS "is_blocked!",
            (CASE
                WHEN up.user_id = $2::uuid THEN 'self'
                WHEN $2::uuid IS NULL THEN 'anonymous'
                WHEN EXISTS (
                    SELECT 1 FROM friend f
                    WHERE f.user_id = up.user_id AND f.friend_id = $2::uuid
                ) THEN 'friends'
                WHEN EXISTS (
                    SELECT 1 FROM friend_request fr
                    WHERE fr.status = 'pending'
                      AND fr.requester_id = $2::uuid AND fr.addressee_id = up.user_id
                ) THEN 'request_sent'
                WHEN EXISTS (
                    SELECT 1 FROM friend_request fr
                    WHERE fr.status = 'pending'
                      AND fr.requester_id = up.user_id AND fr.addressee_id = $2::uuid
                ) THEN 'request_received'
                ELSE 'none'
            END)::TEXT AS "relationship!"
        FROM user_profile up
        INNER JOIN auth_user au ON au.id = up.user_id
        WHERE up.slug = $1 AND au.status = 'active'
        "#,
        slug,
        viewer,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.filter(|row| !row.is_blocked).map(|row| {
        let is_self = viewer == Some(row.user_id);

        ProfileTarget {
            user_id: row.user_id,
            can_view_profile_details: allows(row.profile_visibility, is_self, row.is_friend),
            can_read_scrap: allows(row.scrap_read, is_self, row.is_friend),
            // Escrever exige sessão, então o anônimo nunca pode, mesmo em `everyone`.
            can_write_scrap: viewer.is_some() && allows(row.scrap_write, is_self, row.is_friend),
            // Ninguém escreve depoimento para si mesmo, nem o dono do perfil.
            can_write_testimonial: viewer.is_some()
                && !is_self
                && allows(row.testimonial_write, is_self, row.is_friend),
            // Conversa exige sessão e não faz sentido consigo mesmo.
            can_send_message: viewer.is_some()
                && !is_self
                && allows(row.message_receive, is_self, row.is_friend),
            // Avaliar não tem preferência de privacidade: a amizade é a regra.
            can_rate: !is_self && row.is_friend,
            relationship: row.relationship,
            viewer_is_fan: row.viewer_is_fan,
            show_profile_visits: row.show_profile_visits,
            viewer_shows_visits: row.viewer_shows_visits,
        }
    }))
}

pub fn profile_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "profile",
        "not_found",
        Some("Perfil não encontrado."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}",
    description = "Retorna o perfil público de um usuário ativo pelo slug, com os primeiros amigos quando a privacidade permitir",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    responses(
        (status = 200, description = "Perfil público", body = ApiResponse<ProfileResponse>),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Retorna o perfil público por slug", skip_all)]
pub async fn get(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);
    let slug = slug.into_inner();

    let Some(target) = fetch_target(pool.get_ref(), &slug, viewer).await? else {
        return Ok(profile_not_found());
    };

    // A visita conta só para quem está logado, não é o próprio dono, e com as duas
    // pontas mantendo o histórico ligado. Bloqueio já foi filtrado pelo
    // `fetch_target`, que teria devolvido 404.
    if let Some(visitor_id) = viewer
        && visitor_id != target.user_id
        && target.show_profile_visits
        && target.viewer_shows_visits
    {
        record_visit(&redis, target.user_id, visitor_id).await;
    }

    let profile = sqlx::query!(
        r#"
        SELECT
            slug, name, avatar_url, status, bio, city, birthdate, created_at,
            relationship_status AS "relationship_status: RelationshipStatus",
            country, state, languages, interests, who_i_want_to_meet,
            occupation, company, education, music, movies, books, tv_shows,
            foods, sports, pets, fashion_style,
            -- As contagens ignoram quem não está ativo, senão o número não bate
            -- com a listagem correspondente, que também esconde essa gente.
            (
                SELECT COUNT(*) FROM friend f
                INNER JOIN auth_user au ON au.id = f.friend_id AND au.status = 'active'
                WHERE f.user_id = user_profile.user_id
            ) AS "friends_count!",
            (
                SELECT COUNT(*) FROM scrap s
                INNER JOIN auth_user au ON au.id = s.author_id AND au.status = 'active'
                WHERE s.profile_id = user_profile.user_id
                  AND s.parent_id IS NULL
                  AND s.deleted_at IS NULL
            ) AS "scraps_count!",
            (
                SELECT COUNT(*) FROM testimonial t
                INNER JOIN auth_user au ON au.id = t.author_id AND au.status = 'active'
                WHERE t.profile_id = user_profile.user_id
                  AND t.status = 'accepted'
                  AND t.deleted_at IS NULL
            ) AS "testimonials_count!",
            (
                SELECT COUNT(*) FROM fan
                INNER JOIN auth_user au ON au.id = fan.fan_id AND au.status = 'active'
                WHERE fan.idol_id = user_profile.user_id
            ) AS "fans_count!"
        FROM user_profile
        WHERE user_id = $1
        "#,
        target.user_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    let rating = rating::fetch_summary(pool.get_ref(), target.user_id).await?;

    let my_rating = match viewer {
        Some(viewer) if viewer != target.user_id => {
            rating::fetch_my_rating(pool.get_ref(), viewer, target.user_id).await?
        }
        _ => None,
    };

    let friends = if target.can_view_profile_details {
        friends::fetch_friends(
            pool.get_ref(),
            storage.as_ref().as_ref(),
            target.user_id,
            None,
            None,
            FRIENDS_PREVIEW_LIMIT,
        )
        .await?
    } else {
        Vec::new()
    };

    Ok(HttpResponse::Ok().json(ApiResponse::new(ProfileResponse {
        slug: profile.slug,
        name: profile.name,
        avatar_url: public_url_of(storage.as_ref().as_ref(), profile.avatar_url),
        status: profile.status,
        bio: profile.bio,
        city: profile.city,
        birthdate: profile.birthdate,
        created_at: profile.created_at,
        details: target.can_view_profile_details.then_some(ProfileDetails {
            relationship_status: profile.relationship_status,
            country: profile.country,
            state: profile.state,
            languages: profile.languages,
            interests: profile.interests,
            who_i_want_to_meet: profile.who_i_want_to_meet,
            occupation: profile.occupation,
            company: profile.company,
            education: profile.education,
            music: profile.music,
            movies: profile.movies,
            books: profile.books,
            tv_shows: profile.tv_shows,
            foods: profile.foods,
            sports: profile.sports,
            pets: profile.pets,
            fashion_style: profile.fashion_style,
        }),
        relationship: target.relationship,
        friends_visible: target.can_view_profile_details,
        friends_count: target
            .can_view_profile_details
            .then_some(profile.friends_count),
        friends,
        scraps_visible: target.can_read_scrap,
        can_write_scrap: target.can_write_scrap,
        scraps_count: target.can_read_scrap.then_some(profile.scraps_count),
        can_write_testimonial: target.can_write_testimonial,
        testimonials_visible: target.can_view_profile_details,
        testimonials_count: target
            .can_view_profile_details
            .then_some(profile.testimonials_count),
        can_rate: target.can_rate,
        rating,
        my_rating,
        fans_count: profile.fans_count,
        is_fan: target.viewer_is_fan,
        can_be_fan: viewer.is_some_and(|viewer| viewer != target.user_id),
    })))
}
