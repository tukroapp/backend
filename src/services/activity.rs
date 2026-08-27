//! Registro de atividades para o feed da página inicial. Cada evento vira uma
//! linha em `activity`; o feed é montado na leitura (`routes::feed`).

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "activity_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    TestimonialReceived,
    CommunityJoined,
    CommunityCreated,
    BecameFan,
    FriendshipMade,
    MediaAdded,
    StatusChanged,
}

/// Uma atividade a registrar. Use os construtores em vez de montar à mão: cada
/// `kind` usa um subconjunto dos alvos.
pub struct NewActivity {
    pub actor_id: Uuid,
    pub kind: ActivityKind,
    pub target_user_id: Option<Uuid>,
    pub community_id: Option<Uuid>,
    pub media_id: Option<Uuid>,
    pub testimonial_id: Option<Uuid>,
    pub text_value: Option<String>,
}

impl NewActivity {
    fn base(actor_id: Uuid, kind: ActivityKind) -> Self {
        Self {
            actor_id,
            kind,
            target_user_id: None,
            community_id: None,
            media_id: None,
            testimonial_id: None,
            text_value: None,
        }
    }

    pub fn testimonial_received(recipient: Uuid, author: Uuid, testimonial_id: Uuid) -> Self {
        Self {
            target_user_id: Some(author),
            testimonial_id: Some(testimonial_id),
            ..Self::base(recipient, ActivityKind::TestimonialReceived)
        }
    }

    pub fn community_joined(actor: Uuid, community_id: Uuid) -> Self {
        Self {
            community_id: Some(community_id),
            ..Self::base(actor, ActivityKind::CommunityJoined)
        }
    }

    pub fn community_created(actor: Uuid, community_id: Uuid) -> Self {
        Self {
            community_id: Some(community_id),
            ..Self::base(actor, ActivityKind::CommunityCreated)
        }
    }

    pub fn became_fan(fan: Uuid, idol: Uuid) -> Self {
        Self {
            target_user_id: Some(idol),
            ..Self::base(fan, ActivityKind::BecameFan)
        }
    }

    pub fn friendship_made(accepter: Uuid, requester: Uuid) -> Self {
        Self {
            target_user_id: Some(requester),
            ..Self::base(accepter, ActivityKind::FriendshipMade)
        }
    }

    pub fn media_added(actor: Uuid, media_id: Uuid) -> Self {
        Self {
            media_id: Some(media_id),
            ..Self::base(actor, ActivityKind::MediaAdded)
        }
    }

    pub fn status_changed(actor: Uuid, status: String) -> Self {
        Self {
            text_value: Some(status),
            ..Self::base(actor, ActivityKind::StatusChanged)
        }
    }
}

/// Insere a atividade. Aceita executor genérico para rodar dentro da transação
/// que criou o objeto de origem.
pub async fn record_activity(
    executor: impl PgExecutor<'_>,
    activity: NewActivity,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO activity
            (actor_id, kind, target_user_id, community_id, media_id, testimonial_id, text_value)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
        activity.actor_id,
        activity.kind as ActivityKind,
        activity.target_user_id,
        activity.community_id,
        activity.media_id,
        activity.testimonial_id,
        activity.text_value,
    )
    .execute(executor)
    .await?;

    Ok(())
}
