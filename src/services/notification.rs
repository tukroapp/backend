use sqlx::PgExecutor;
use uuid::Uuid;

/// Os tipos que o backend emite hoje.
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(type_name = "notification_kind", rename_all = "snake_case")]
pub enum NotificationKind {
    FriendRequest,
    FriendAccepted,
    ScrapReceived,
    ScrapReply,
    TestimonialReceived,
    TestimonialAccepted,
    CommunityJoinRequest,
    CommunityJoinAccepted,
    TopicComment,
    TopicReply,
    NewFan,
}

/// O que originou a notificação. A tabela tem uma coluna por tipo de alvo, e
/// só uma delas é preenchida em cada linha.
#[derive(Debug, Clone, Copy)]
pub enum NotificationTarget {
    Scrap(Uuid),
    Testimonial(Uuid),
    FriendRequest(Uuid),
    Community(Uuid),
    Topic(Uuid),
    /// A resposta carrega o tópico junto porque o link do aviso abre o
    /// comentário dentro do tópico, e não o comentário solto.
    Comment {
        topic_id: Uuid,
        comment_id: Uuid,
    },
}

/// Cria a notificação. Vira no-op quando o ator é o próprio destinatário, o que
/// acontece o tempo todo (escrever no próprio mural, aceitar o próprio pedido).
///
/// Recebe um executor genérico para poder ser chamada dentro das transações que
/// já existem, junto da escrita que originou o aviso.
pub async fn notify(
    executor: impl PgExecutor<'_>,
    user_id: Uuid,
    actor_id: Uuid,
    kind: NotificationKind,
    target: NotificationTarget,
) -> Result<(), sqlx::Error> {
    if user_id == actor_id {
        return Ok(());
    }

    let (scrap_id, testimonial_id, friend_request_id, community_id, topic_id, comment_id) =
        match target {
            NotificationTarget::Scrap(id) => (Some(id), None, None, None, None, None),
            NotificationTarget::Testimonial(id) => (None, Some(id), None, None, None, None),
            NotificationTarget::FriendRequest(id) => (None, None, Some(id), None, None, None),
            NotificationTarget::Community(id) => (None, None, None, Some(id), None, None),
            NotificationTarget::Topic(id) => (None, None, None, None, Some(id), None),
            NotificationTarget::Comment {
                topic_id,
                comment_id,
            } => (None, None, None, None, Some(topic_id), Some(comment_id)),
        };

    sqlx::query!(
        r#"
        INSERT INTO notification (user_id, actor_id, kind, scrap_id, testimonial_id, friend_request_id, community_id, topic_id, comment_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
        user_id,
        actor_id,
        kind as NotificationKind,
        scrap_id,
        testimonial_id,
        friend_request_id,
        community_id,
        topic_id,
        comment_id,
    )
    .execute(executor)
    .await?;

    Ok(())
}

/// Avisa dono e admins da comunidade de uma vez. O `notify` acima é de um
/// destinatário só, e aqui a moderação é compartilhada.
pub async fn notify_community_moderators(
    executor: impl PgExecutor<'_>,
    community_id: Uuid,
    actor_id: Uuid,
    kind: NotificationKind,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO notification (user_id, actor_id, kind, community_id)
        SELECT cm.user_id, $2, $3, $1
        FROM community_member cm
        WHERE cm.community_id = $1
          AND cm.role IN ('owner', 'admin')
          AND cm.user_id <> $2
        "#,
        community_id,
        actor_id,
        kind as NotificationKind,
    )
    .execute(executor)
    .await?;

    Ok(())
}

/// Avisa que apareceu gente nova querendo ser fã. Diferente das outras, não é
/// uma notificação por evento: enquanto o ídolo não ler a que já está lá,
/// nenhuma outra é criada. Sem isso, um perfil que viraliza colocaria uma linha
/// por fã na caixa de uma pessoa só, e o contador de não lidas do polling
/// passaria a varrer tudo isso a cada consulta.
pub async fn notify_new_fan(
    executor: impl PgExecutor<'_>,
    idol_id: Uuid,
    fan_id: Uuid,
) -> Result<(), sqlx::Error> {
    if idol_id == fan_id {
        return Ok(());
    }

    sqlx::query!(
        r#"
        INSERT INTO notification (user_id, actor_id, kind)
        SELECT $1, $2, 'new_fan'
        WHERE NOT EXISTS (
            SELECT 1 FROM notification
            WHERE user_id = $1 AND kind = 'new_fan' AND read_at IS NULL
        )
        "#,
        idol_id,
        fan_id,
    )
    .execute(executor)
    .await?;

    Ok(())
}
