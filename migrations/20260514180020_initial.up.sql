CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE EXTENSION IF NOT EXISTS citext;

CREATE OR REPLACE FUNCTION set_updated_at()
    RETURNS TRIGGER
    AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION set_updated_at_for(table_name TEXT)
    RETURNS VOID
    AS $$
BEGIN
    EXECUTE FORMAT('
        DROP TRIGGER IF EXISTS trg_%I_set_updated_at ON %I;
        CREATE TRIGGER trg_%I_set_updated_at
        BEFORE UPDATE ON %I
        FOR EACH ROW
        EXECUTE FUNCTION set_updated_at();', table_name, table_name, table_name, table_name);
END;
$$
LANGUAGE plpgsql;

CREATE TYPE token_type AS ENUM(
    'email_verification',
    'password_reset'
);

CREATE TYPE account_status AS ENUM(
    'active',
    'suspended',
    -- Saída voluntária, reversível: o próprio login devolve a conta ao ar.
    'deactivated',
    -- Conta já anonimizada. Não volta, e o login recusa.
    'deleted'
);

CREATE TYPE visibility AS ENUM(
    'everyone',
    'friends',
    'nobody'
);

CREATE TYPE relationship_status AS ENUM(
    'single',
    'dating',
    'engaged',
    'married',
    'stable_union',
    'open_relationship',
    'complicated',
    'separated',
    'widowed'
);

CREATE TYPE approval_status AS ENUM(
    'pending',
    'accepted',
    'rejected'
);

CREATE TYPE community_role AS ENUM(
    'member',
    'admin',
    'owner'
);

CREATE TYPE media_kind AS ENUM(
    'photo',
    'video'
);

CREATE TYPE notification_kind AS ENUM(
    'friend_request',
    'friend_accepted',
    'scrap_received',
    'scrap_reply',
    'testimonial_received',
    'testimonial_accepted',
    'community_join_request',
    'community_join_accepted',
    'topic_comment',
    'topic_reply'
);

CREATE TYPE report_reason AS ENUM(
    'spam',
    'harassment',
    'hate_speech',
    'nudity',
    'impersonation',
    'other'
);

CREATE TYPE report_status AS ENUM(
    'open',
    'reviewing',
    'resolved',
    'dismissed'
);

CREATE TYPE moderation_action AS ENUM(
    'content_removed',
    'content_restored',
    'user_suspended',
    'user_unsuspended'
);

CREATE TYPE activity_kind AS ENUM(
    'testimonial_received',
    'community_joined',
    'community_created',
    'became_fan',
    'friendship_made',
    'media_added',
    'status_changed'
);

CREATE TABLE auth_user(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    status account_status NOT NULL DEFAULT 'active',
    status_reason TEXT,
    -- Só a suspensão tem prazo. NULL enquanto suspenso significa permanente.
    suspended_until TIMESTAMPTZ DEFAULT NULL,
    status_changed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    status_changed_by UUID REFERENCES auth_user(id) ON DELETE SET NULL,
    is_superuser BOOLEAN NOT NULL DEFAULT FALSE,
    is_email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    email CITEXT NOT NULL UNIQUE,
    password_hash TEXT,
    google_id TEXT,
    last_login TIMESTAMPTZ DEFAULT NULL,
    -- Exclusão definitiva pedida pelo usuário, com prazo para ele se arrepender.
    -- A conta fica desativada até lá; o worker anonimiza quando vence.
    deletion_scheduled_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- A conta anonimizada perde as duas credenciais, então fica fora da regra.
    CONSTRAINT check_auth_user_credential CHECK (status = 'deleted'
        OR password_hash IS NOT NULL
        OR google_id IS NOT NULL),
    CONSTRAINT check_auth_user_suspension CHECK (status = 'suspended' OR suspended_until IS NULL)
);

SELECT
    set_updated_at_for('auth_user');

CREATE UNIQUE INDEX idx_auth_user_google_id ON auth_user(google_id)
WHERE
    google_id IS NOT NULL;

-- A fila do worker de exclusão.
CREATE INDEX idx_auth_user_deletion_due ON auth_user(deletion_scheduled_at)
WHERE
    deletion_scheduled_at IS NOT NULL;

CREATE TABLE user_profile(
    user_id UUID PRIMARY KEY REFERENCES auth_user(id) ON DELETE CASCADE,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    avatar_url TEXT,
    status TEXT,
    bio TEXT,
    city TEXT,
    birthdate DATE,
    -- Perfil detalhado, no estilo das abas social, profissional e pessoal do
    -- Orkut. Tudo é opcional: as listas chegam vazias e os textos NULL.
    relationship_status relationship_status,
    country TEXT,
    state TEXT,
    languages TEXT[] NOT NULL DEFAULT '{}',
    interests TEXT[] NOT NULL DEFAULT '{}',
    who_i_want_to_meet TEXT,
    occupation TEXT,
    company TEXT,
    education TEXT,
    music TEXT[] NOT NULL DEFAULT '{}',
    movies TEXT[] NOT NULL DEFAULT '{}',
    books TEXT[] NOT NULL DEFAULT '{}',
    tv_shows TEXT[] NOT NULL DEFAULT '{}',
    foods TEXT[] NOT NULL DEFAULT '{}',
    sports TEXT[] NOT NULL DEFAULT '{}',
    pets TEXT,
    fashion_style TEXT,
    profile_visibility visibility NOT NULL DEFAULT 'everyone',
    scrap_read visibility NOT NULL DEFAULT 'everyone',
    scrap_write visibility NOT NULL DEFAULT 'friends',
    testimonial_write visibility NOT NULL DEFAULT 'friends',
    -- Quem pode iniciar uma conversa privada com esse usuário.
    message_receive visibility NOT NULL DEFAULT 'friends',
    -- Toggle único, no estilo Orkut: TRUE mostra quem visitou o perfil e deixa as
    -- visitas dessa pessoa visíveis nos perfis que ela visita. FALSE corta os dois
    -- lados: nenhuma visita dela é registrada e ela não vê a própria lista.
    show_profile_visits BOOLEAN NOT NULL DEFAULT TRUE,
    -- Até onde o usuário já viu o feed. O contador de "novas" no /me/unread conta
    -- o que passou disso.
    feed_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_user_profile_slug_format CHECK (slug ~ '^[a-z0-9]([a-z0-9-]{1,28}[a-z0-9])$'),
    CONSTRAINT check_user_profile_name_length CHECK (CHAR_LENGTH(NAME) BETWEEN 1 AND 80),
    CONSTRAINT check_user_profile_list_size CHECK (CARDINALITY(languages) <= 20
        AND CARDINALITY(interests) <= 30
        AND CARDINALITY(music) <= 30
        AND CARDINALITY(movies) <= 30
        AND CARDINALITY(books) <= 30
        AND CARDINALITY(tv_shows) <= 30
        AND CARDINALITY(foods) <= 30
        AND CARDINALITY(sports) <= 30)
);

SELECT
    set_updated_at_for('user_profile');

CREATE TABLE verification_token(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    token_hash TEXT NOT NULL,
    kind token_type NOT NULL,
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    used_at TIMESTAMPTZ DEFAULT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_expiry_future CHECK (expires_at > created_at)
);

CREATE INDEX idx_verification_lookup ON verification_token(token_hash, kind, user_id)
WHERE
    used_at IS NULL;

CREATE INDEX idx_verification_expiry ON verification_token(expires_at);

CREATE INDEX idx_verification_user_recent ON verification_token(user_id, created_at DESC);

CREATE TABLE email_queue(
    id BIGSERIAL PRIMARY KEY,
    from_address TEXT NOT NULL,
    to_address TEXT[] NOT NULL,
    cc TEXT[] NOT NULL DEFAULT '{}',
    bcc TEXT[] NOT NULL DEFAULT '{}',
    subject TEXT NOT NULL,
    html_content TEXT NOT NULL,
    text_content TEXT NOT NULL,
    attempts SMALLINT NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_email_queue_pending ON email_queue(scheduled_at)
WHERE
    attempts < 5;

CREATE TABLE email_log(
    id BIGSERIAL PRIMARY KEY,
    from_address TEXT NOT NULL,
    to_address TEXT[] NOT NULL,
    cc TEXT[] NOT NULL DEFAULT '{}',
    bcc TEXT[] NOT NULL DEFAULT '{}',
    subject TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('sent', 'failed')),
    error_msg TEXT,
    attempts SMALLINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_email_log_sent_at ON email_log(sent_at);

CREATE INDEX idx_user_profile_name_trgm ON user_profile USING GIN(name gin_trgm_ops);

CREATE INDEX idx_user_profile_city_trgm ON user_profile USING GIN(city gin_trgm_ops);

CREATE TABLE user_media(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    kind media_kind NOT NULL,
    -- Key do objeto no bucket. O host nunca é persistido: muda entre ambientes
    -- e é montado na leitura.
    storage_key TEXT NOT NULL UNIQUE,
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_user_media_size CHECK (size_bytes > 0)
);

SELECT
    set_updated_at_for('user_media');

CREATE INDEX idx_user_media_gallery ON user_media(user_id, kind, id DESC);

CREATE TABLE friend_request(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    requester_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    addressee_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    status approval_status NOT NULL DEFAULT 'pending',
    responded_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_friend_request_not_self CHECK (requester_id <> addressee_id),
    CONSTRAINT check_friend_request_responded CHECK ((status = 'pending') =(responded_at IS NULL))
);

CREATE UNIQUE INDEX idx_friend_request_pending ON friend_request(requester_id, addressee_id)
WHERE
    status = 'pending';

CREATE INDEX idx_friend_request_inbox ON friend_request(addressee_id, created_at DESC)
WHERE
    status = 'pending';

CREATE TABLE friend(
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    friend_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    -- Posição no "top amigos" do dono da linha, de 1 a 9 como a grade do Orkut.
    -- NULL é o normal: só quem a pessoa destacou tem posição.
    top_position SMALLINT DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, friend_id),
    CONSTRAINT check_friend_not_self CHECK (user_id <> friend_id),
    CONSTRAINT check_friend_top_position CHECK (top_position IS NULL
        OR top_position BETWEEN 1 AND 9)
);

-- Uma posição por pessoa: a lista é uma ordem, não um conjunto de empates.
CREATE UNIQUE INDEX idx_friend_top_position ON friend(user_id, top_position)
WHERE
    top_position IS NOT NULL;

-- "Sou fã": relação unilateral, sem aprovação, sem exigir amizade. A linha
-- presente é o estado; ativar/desativar é insert/delete.
CREATE TABLE fan(
    fan_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    idol_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (fan_id, idol_id),
    CONSTRAINT check_fan_not_self CHECK (fan_id <> idol_id)
);

CREATE INDEX idx_fan_idol ON fan(idol_id, created_at DESC);

CREATE TABLE block(
    blocker_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    blocked_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (blocker_id, blocked_id),
    CONSTRAINT check_block_not_self CHECK (blocker_id <> blocked_id)
);

CREATE INDEX idx_block_blocked ON block(blocked_id);

CREATE TABLE scrap(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    profile_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    parent_id UUID DEFAULT NULL,
    body TEXT NOT NULL,
    deleted_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_scrap_body_length CHECK (CHAR_LENGTH(body) BETWEEN 1 AND 2000),
    CONSTRAINT check_scrap_not_own_parent CHECK (parent_id IS NULL OR parent_id <> id),
    CONSTRAINT uq_scrap_id_profile UNIQUE (id, profile_id),
    CONSTRAINT fk_scrap_parent FOREIGN KEY (parent_id, profile_id) REFERENCES scrap(id, profile_id) ON DELETE CASCADE
);

SELECT
    set_updated_at_for('scrap');

CREATE INDEX idx_scrap_wall ON scrap(profile_id, created_at DESC)
WHERE
    parent_id IS NULL AND deleted_at IS NULL;

CREATE INDEX idx_scrap_thread ON scrap(parent_id, created_at)
WHERE
    parent_id IS NOT NULL AND deleted_at IS NULL;

CREATE INDEX idx_scrap_author ON scrap(author_id, created_at DESC);

CREATE TABLE testimonial(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    profile_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    status approval_status NOT NULL DEFAULT 'pending',
    responded_at TIMESTAMPTZ DEFAULT NULL,
    deleted_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_testimonial_not_self CHECK (profile_id <> author_id),
    CONSTRAINT check_testimonial_body_length CHECK (CHAR_LENGTH(body) BETWEEN 1 AND 5000),
    CONSTRAINT check_testimonial_responded CHECK ((status = 'pending') =(responded_at IS NULL))
);

SELECT
    set_updated_at_for('testimonial');

CREATE INDEX idx_testimonial_accepted ON testimonial(profile_id, created_at DESC)
WHERE
    status = 'accepted' AND deleted_at IS NULL;

CREATE INDEX idx_testimonial_pending ON testimonial(profile_id, created_at DESC)
WHERE
    status = 'pending' AND deleted_at IS NULL;

-- Zero num eixo significa "não opinei sobre esse", e a linha toda zerada não é
-- avaliação nenhuma.
CREATE TABLE profile_rating(
    rater_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    rated_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    trusty SMALLINT NOT NULL,
    cool SMALLINT NOT NULL,
    sexy SMALLINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (rater_id, rated_id),
    CONSTRAINT check_profile_rating_not_self CHECK (rater_id <> rated_id),
    CONSTRAINT check_profile_rating_range CHECK (
        trusty BETWEEN 0 AND 3
        AND cool BETWEEN 0 AND 3
        AND sexy BETWEEN 0 AND 3
    ),
    CONSTRAINT check_profile_rating_not_empty CHECK (trusty + cool + sexy > 0)
);

SELECT
    set_updated_at_for('profile_rating');

CREATE INDEX idx_profile_rating_rated ON profile_rating(rated_id);

CREATE TABLE community(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    avatar_url TEXT,
    is_private BOOLEAN NOT NULL DEFAULT FALSE,
    -- Remoção pela moderação. O dono continua excluindo de verdade pelo próprio
    -- endpoint; aqui a comunidade só sai do ar e pode voltar.
    deleted_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_community_slug_format CHECK (slug ~ '^[a-z0-9]([a-z0-9-]{1,48}[a-z0-9])$'),
    CONSTRAINT check_community_name_length CHECK (CHAR_LENGTH(NAME) BETWEEN 1 AND 100)
);

SELECT
    set_updated_at_for('community');

CREATE INDEX idx_community_name_trgm ON community USING GIN(name gin_trgm_ops);

CREATE TABLE community_member(
    community_id UUID NOT NULL REFERENCES community(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    ROLE community_role NOT NULL DEFAULT 'member',
    joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, user_id)
);

CREATE INDEX idx_community_member_user ON community_member(user_id, joined_at DESC);

CREATE UNIQUE INDEX idx_community_single_owner ON community_member(community_id)
WHERE
    ROLE = 'owner';

CREATE TABLE community_join_request(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    community_id UUID NOT NULL REFERENCES community(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    status approval_status NOT NULL DEFAULT 'pending',
    responded_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_community_join_responded CHECK ((status = 'pending') =(responded_at IS NULL))
);

CREATE UNIQUE INDEX idx_community_join_pending ON community_join_request(community_id, user_id)
WHERE
    status = 'pending';

CREATE TABLE community_topic(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    community_id UUID NOT NULL REFERENCES community(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    is_pinned BOOLEAN NOT NULL DEFAULT FALSE,
    is_locked BOOLEAN NOT NULL DEFAULT FALSE,
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- updated_at não serve para marcar edição: a trigger dispara também no bump
    -- de last_activity_at, em pin e em lock. Só o handler de edição escreve aqui.
    edited_at TIMESTAMPTZ DEFAULT NULL,
    deleted_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_community_topic_title_length CHECK (CHAR_LENGTH(title) BETWEEN 1 AND 200),
    CONSTRAINT check_community_topic_body_length CHECK (CHAR_LENGTH(body) BETWEEN 1 AND 20000)
);

SELECT
    set_updated_at_for('community_topic');

CREATE INDEX idx_community_topic_listing ON community_topic(community_id, is_pinned DESC, last_activity_at DESC)
WHERE
    deleted_at IS NULL;

-- O corpo de um comentário apagado continua na linha: serve de evidência para a
-- moderação. Quem troca o texto por um marcador é a serialização da API.
CREATE TABLE community_topic_comment(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    topic_id UUID NOT NULL REFERENCES community_topic(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    parent_id UUID DEFAULT NULL,
    body TEXT NOT NULL,
    edited_at TIMESTAMPTZ DEFAULT NULL,
    deleted_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_community_topic_comment_body_length CHECK (CHAR_LENGTH(body) BETWEEN 1 AND 20000),
    CONSTRAINT check_community_topic_comment_not_own_parent CHECK (parent_id IS NULL OR parent_id <> id),
    CONSTRAINT uq_community_topic_comment_id_topic UNIQUE (id, topic_id),
    CONSTRAINT fk_community_topic_comment_parent FOREIGN KEY (parent_id, topic_id) REFERENCES community_topic_comment(id, topic_id) ON DELETE CASCADE
);

SELECT
    set_updated_at_for('community_topic_comment');

-- Nenhum dos dois filtra deleted_at: comentário apagado continua na thread como
-- marcador, senão a paginação encolhe e as respostas perdem o contexto.
CREATE INDEX idx_community_topic_comment_root ON community_topic_comment(topic_id, created_at)
WHERE
    parent_id IS NULL;

CREATE INDEX idx_community_topic_comment_reply ON community_topic_comment(parent_id, created_at)
WHERE
    parent_id IS NOT NULL;

CREATE OR REPLACE FUNCTION bump_topic_activity()
    RETURNS TRIGGER
    AS $$
BEGIN
    UPDATE
        community_topic
    SET
        last_activity_at = NEW.created_at
    WHERE
        id = NEW.topic_id;
    RETURN NEW;
END;
$$
LANGUAGE plpgsql;

CREATE TRIGGER trg_community_topic_comment_bump_activity
    AFTER INSERT ON community_topic_comment
    FOR EACH ROW
    EXECUTE FUNCTION bump_topic_activity();

CREATE TABLE notification(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    actor_id UUID REFERENCES auth_user(id) ON DELETE CASCADE,
    kind notification_kind NOT NULL,
    scrap_id UUID REFERENCES scrap(id) ON DELETE CASCADE,
    testimonial_id UUID REFERENCES testimonial(id) ON DELETE CASCADE,
    friend_request_id UUID REFERENCES friend_request(id) ON DELETE CASCADE,
    community_id UUID REFERENCES community(id) ON DELETE CASCADE,
    topic_id UUID REFERENCES community_topic(id) ON DELETE CASCADE,
    comment_id UUID REFERENCES community_topic_comment(id) ON DELETE CASCADE,
    read_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_notification_not_self CHECK (actor_id IS NULL OR actor_id <> user_id)
);

CREATE INDEX idx_notification_unread ON notification(user_id, created_at DESC)
WHERE
    read_at IS NULL;

CREATE INDEX idx_notification_inbox ON notification(user_id, created_at DESC);

-- Enquete de comunidade. Vive fora do fórum: criação restrita à moderação, voto
-- restrito a membros, e fechamento fixo em 24h (ver closes_at).
CREATE TABLE community_poll(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    community_id UUID NOT NULL REFERENCES community(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    question TEXT NOT NULL,
    -- min: 1, max: 1 é voto único. min: 1, max: 2 deixa marcar uma ou duas
    -- opções. A validação de max contra a quantidade de opções fica no handler.
    min_choices SMALLINT NOT NULL DEFAULT 1,
    max_choices SMALLINT NOT NULL DEFAULT 1,
    -- Fechamento manual antecipado pela moderação. O fechamento por tempo é
    -- decidido na leitura, comparando NOW() com closes_at.
    is_closed BOOLEAN NOT NULL DEFAULT FALSE,
    -- Fixado em NOW() + 24h na criação e imutável. Quem escreve é o handler.
    closes_at TIMESTAMPTZ NOT NULL,
    -- Preenchido só quando o autor reescreve a pergunta. `updated_at` não serve:
    -- a trigger dispara também no encerramento manual.
    edited_at TIMESTAMPTZ DEFAULT NULL,
    deleted_at TIMESTAMPTZ DEFAULT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_community_poll_question_length CHECK (CHAR_LENGTH(question) BETWEEN 1 AND 300),
    CONSTRAINT check_community_poll_choices CHECK (min_choices >= 1 AND max_choices >= min_choices)
);

SELECT
    set_updated_at_for('community_poll');

CREATE INDEX idx_community_poll_listing ON community_poll(community_id, created_at DESC)
WHERE
    deleted_at IS NULL;

CREATE TABLE community_poll_option(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    poll_id UUID NOT NULL REFERENCES community_poll(id) ON DELETE CASCADE,
    position SMALLINT NOT NULL,
    text TEXT NOT NULL,
    CONSTRAINT check_community_poll_option_text_length CHECK (CHAR_LENGTH(text) BETWEEN 1 AND 120),
    CONSTRAINT uq_community_poll_option_position UNIQUE (poll_id, position),
    CONSTRAINT uq_community_poll_option_id_poll UNIQUE (id, poll_id)
);

CREATE INDEX idx_community_poll_option_poll ON community_poll_option(poll_id, position);

-- Uma linha por opção marcada. A PK garante voto único por pessoa; quando
-- max_choices > 1 a pessoa tem mais de uma linha. A FK composta amarra a opção à
-- enquete e faz o voto sumir junto com a opção ou com a enquete.
CREATE TABLE community_poll_vote(
    poll_id UUID NOT NULL,
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    option_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (poll_id, user_id, option_id),
    CONSTRAINT fk_community_poll_vote_option FOREIGN KEY (option_id, poll_id) REFERENCES community_poll_option(id, poll_id) ON DELETE CASCADE
);

CREATE INDEX idx_community_poll_vote_tally ON community_poll_vote(poll_id, option_id);

CREATE TABLE report(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    reporter_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    reason report_reason NOT NULL,
    details TEXT,
    status report_status NOT NULL DEFAULT 'open',
    reviewed_by UUID REFERENCES auth_user(id) ON DELETE SET NULL,
    reviewed_at TIMESTAMPTZ DEFAULT NULL,
    reported_user_id UUID REFERENCES auth_user(id) ON DELETE CASCADE,
    scrap_id UUID REFERENCES scrap(id) ON DELETE CASCADE,
    testimonial_id UUID REFERENCES testimonial(id) ON DELETE CASCADE,
    community_id UUID REFERENCES community(id) ON DELETE CASCADE,
    topic_id UUID REFERENCES community_topic(id) ON DELETE CASCADE,
    comment_id UUID REFERENCES community_topic_comment(id) ON DELETE CASCADE,
    poll_id UUID REFERENCES community_poll(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_report_single_target CHECK (num_nonnulls(reported_user_id, scrap_id, testimonial_id, community_id, topic_id, comment_id, poll_id) = 1)
);

CREATE INDEX idx_report_queue ON report(created_at)
WHERE
    status = 'open';

-- Uma denúncia de perfil por denunciante, para não virar enxurrada. Só vale
-- para denúncia de perfil: os outros alvos podem repetir.
CREATE UNIQUE INDEX idx_report_unique_reported_user ON report(reporter_id, reported_user_id)
WHERE
    reported_user_id IS NOT NULL;

-- Mensagens privadas em estilo chat. Uma conversa por par de usuários; o par é
-- guardado ordenado (lo < hi) para o UNIQUE valer nos dois sentidos. O estado de
-- leitura e o "esconder" ficam por participante.
CREATE TABLE conversation(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    user_lo UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    user_hi UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    -- Denormalizado para ordenar e montar a lista sem varrer as mensagens. Quem
    -- escreve é a trigger no INSERT de message.
    last_message_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_message_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_conversation_pair_order CHECK (user_lo < user_hi),
    CONSTRAINT uq_conversation_pair UNIQUE (user_lo, user_hi)
);

CREATE INDEX idx_conversation_recent ON conversation(last_message_at DESC, id DESC);

CREATE TABLE conversation_participant(
    conversation_id UUID NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    -- '-infinity' e não NOW(): a primeira mensagem tem o mesmo timestamp da
    -- criação da linha e precisa contar como não lida.
    last_read_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    -- "Excluir conversa" do meu lado. A conversa reaparece sozinha quando chega
    -- mensagem nova (last_message_at > hidden_at).
    hidden_at TIMESTAMPTZ DEFAULT NULL,
    PRIMARY KEY (conversation_id, user_id)
);

CREATE INDEX idx_conversation_participant_user ON conversation_participant(user_id);

CREATE TABLE message(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    conversation_id UUID NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
    sender_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT check_message_body_length CHECK (CHAR_LENGTH(body) BETWEEN 1 AND 10000)
);

CREATE INDEX idx_message_conversation ON message(conversation_id, created_at DESC, id DESC);

ALTER TABLE conversation
    ADD CONSTRAINT fk_conversation_last_message FOREIGN KEY (last_message_id) REFERENCES message(id) ON DELETE SET NULL;

CREATE OR REPLACE FUNCTION bump_conversation()
    RETURNS TRIGGER
    AS $$
BEGIN
    UPDATE
        conversation
    SET
        last_message_at = NEW.created_at,
        last_message_id = NEW.id
    WHERE
        id = NEW.conversation_id;
    RETURN NEW;
END;
$$
LANGUAGE plpgsql;

CREATE TRIGGER trg_message_bump_conversation
    AFTER INSERT ON message
    FOR EACH ROW
    EXECUTE FUNCTION bump_conversation();

-- Log único de atividades para o feed da página inicial. O feed é montado na
-- leitura filtrando por quem o usuário segue (amigos e ídolos). Cada handler que
-- cria a coisa insere uma linha aqui; os alvos usados dependem do `kind`.
CREATE TABLE activity(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    actor_id UUID NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
    kind activity_kind NOT NULL,
    target_user_id UUID REFERENCES auth_user(id) ON DELETE CASCADE,
    community_id UUID REFERENCES community(id) ON DELETE CASCADE,
    media_id UUID REFERENCES user_media(id) ON DELETE CASCADE,
    testimonial_id UUID REFERENCES testimonial(id) ON DELETE CASCADE,
    text_value TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_activity_actor ON activity(actor_id, created_at DESC, id DESC);

CREATE INDEX idx_activity_recent ON activity(created_at DESC, id DESC);

-- Histórico da moderação. As colunas de alvo repetem as de `report`, mas com
-- ON DELETE SET NULL: o registro precisa sobreviver ao conteúdo que descreve.
-- É por isso que existe o `content_snapshot`, que guarda o texto removido.
-- Ao contrário de `report`, aqui não há alvo único: a remoção de um conteúdo
-- preenche a coluna do conteúdo e também `target_user_id`, que é o autor, para
-- o histórico por conta sair numa consulta só.
CREATE TABLE moderation_log(
    id UUID NOT NULL PRIMARY KEY DEFAULT uuidv7(),
    moderator_id UUID REFERENCES auth_user(id) ON DELETE SET NULL,
    action moderation_action NOT NULL,
    -- A denúncia que originou a ação, quando houve uma.
    report_id UUID REFERENCES report(id) ON DELETE SET NULL,
    reason TEXT,
    content_snapshot TEXT,
    target_user_id UUID REFERENCES auth_user(id) ON DELETE SET NULL,
    scrap_id UUID REFERENCES scrap(id) ON DELETE SET NULL,
    testimonial_id UUID REFERENCES testimonial(id) ON DELETE SET NULL,
    community_id UUID REFERENCES community(id) ON DELETE SET NULL,
    topic_id UUID REFERENCES community_topic(id) ON DELETE SET NULL,
    comment_id UUID REFERENCES community_topic_comment(id) ON DELETE SET NULL,
    poll_id UUID REFERENCES community_poll(id) ON DELETE SET NULL,
    -- Cópia do slug e do nome de quem sofreu a ação, para o histórico continuar
    -- legível depois que a conta ou a comunidade some.
    target_label TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_moderation_log_recent ON moderation_log(created_at DESC, id DESC);

CREATE INDEX idx_moderation_log_target_user ON moderation_log(target_user_id, created_at DESC)
WHERE
    target_user_id IS NOT NULL;

CREATE INDEX idx_moderation_log_report ON moderation_log(report_id)
WHERE
    report_id IS NOT NULL;
