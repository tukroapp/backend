DROP TABLE IF EXISTS activity;

DROP TABLE IF EXISTS message CASCADE;

DROP TABLE IF EXISTS conversation_participant;

DROP TABLE IF EXISTS conversation;

DROP FUNCTION IF EXISTS bump_conversation;

DROP TABLE IF EXISTS report;

DROP TABLE IF EXISTS community_poll_vote;

DROP TABLE IF EXISTS community_poll_option;

DROP TABLE IF EXISTS community_poll;

DROP TABLE IF EXISTS notification;

DROP TABLE IF EXISTS community_topic_comment;

DROP TABLE IF EXISTS community_topic;

DROP TABLE IF EXISTS community_join_request;

DROP TABLE IF EXISTS community_member;

DROP TABLE IF EXISTS community;

DROP TABLE IF EXISTS profile_rating;

DROP TABLE IF EXISTS testimonial;

DROP TABLE IF EXISTS scrap;

DROP TABLE IF EXISTS block;

DROP TABLE IF EXISTS fan;

DROP TABLE IF EXISTS friend;

DROP TABLE IF EXISTS friend_request;

DROP TABLE IF EXISTS email_log;

DROP TABLE IF EXISTS email_queue;

DROP TABLE IF EXISTS verification_token;

DROP TABLE IF EXISTS user_media;

DROP TABLE IF EXISTS user_profile;

DROP TABLE IF EXISTS auth_user;

DROP FUNCTION IF EXISTS bump_topic_activity;

DROP FUNCTION IF EXISTS set_updated_at_for;

DROP FUNCTION IF EXISTS set_updated_at;

DROP TYPE IF EXISTS activity_kind;

DROP TYPE IF EXISTS report_status;

DROP TYPE IF EXISTS report_reason;

DROP TYPE IF EXISTS notification_kind;

DROP TYPE IF EXISTS media_kind;

DROP TYPE IF EXISTS community_role;

DROP TYPE IF EXISTS approval_status;

DROP TYPE IF EXISTS relationship_status;

DROP TYPE IF EXISTS visibility;

DROP TYPE IF EXISTS account_status;

DROP TYPE IF EXISTS token_type;

DROP EXTENSION IF EXISTS citext;

DROP EXTENSION IF EXISTS pg_trgm;
