use std::sync::Arc;

use sqlx::PgPool;

use crate::constants::{
    EMAIL_BATCH_SIZE, EMAIL_MAX_CONCURRENT, EMAIL_POLL_INTERVAL, FROM_EMAIL_ADDRESS,
};
use askama::Template;
use html2text::from_read;

use futures::stream::StreamExt;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{
        Mailboxes, MultiPart, SinglePart,
        header::{Bcc, Cc, ContentType, To},
    },
    transport::smtp::authentication::Credentials,
};
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, instrument, warn};

pub struct EmailClient;

impl EmailClient {
    pub async fn queue_email<T: Template>(
        pool: &PgPool,
        to: Vec<String>,
        cc: Option<Vec<String>>,
        bcc: Option<Vec<String>>,
        subject: &str,
        template: T,
    ) -> Result<(), anyhow::Error> {
        let cc = cc.unwrap_or_default();
        let bcc = bcc.unwrap_or_default();

        let html = template.render()?;
        let text = from_read(html.as_bytes(), 120)?;

        sqlx::query!(
            r#"
            INSERT INTO email_queue (
                from_address,
                to_address,
                cc,
                bcc,
                subject,
                html_content,
                text_content
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            FROM_EMAIL_ADDRESS,
            &to,
            &cc,
            &bcc,
            format!("[Tukro] {}", subject),
            html,
            text,
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn queue_email_single<T: Template>(
        pool: &PgPool,
        to: &str,
        subject: &str,
        template: T,
    ) -> Result<(), anyhow::Error> {
        Self::queue_email(pool, vec![to.to_string()], None, None, subject, template).await
    }
}

const MAX_ATTEMPTS: i16 = 5;
const CLAIM_TTL_MINUTES: i64 = 5;

#[derive(Debug)]
struct EmailQueue {
    id: i64,
    from_address: String,
    to_address: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
    html_content: String,
    text_content: String,
    attempts: i16,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct EmailWorker {
    pool: PgPool,
    smtp_host: String,
    smtp_port: u16,
    smtp_username: String,
    smtp_password: String,
}

impl EmailWorker {
    pub fn new(
        pool: PgPool,
        smtp_host: String,
        smtp_port: u16,
        smtp_username: String,
        smtp_password: String,
    ) -> Self {
        Self {
            pool,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
        }
    }

    pub async fn run(self) -> Result<(), anyhow::Error> {
        let credentials = Credentials::new(self.smtp_username.clone(), self.smtp_password.clone());

        let mailer: AsyncSmtpTransport<Tokio1Executor> =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(self.smtp_host.as_str())?
                .credentials(credentials)
                .port(self.smtp_port)
                .timeout(Some(Duration::from_secs(15)))
                .build();

        let this = Arc::new(self);

        info!(
            batch_size = EMAIL_BATCH_SIZE,
            max_concurrent = EMAIL_MAX_CONCURRENT,
            "email worker started"
        );

        loop {
            let claimed = match this.claim_batch().await {
                Ok(rows) => rows,
                Err(e) => {
                    error!(%e, "failed to claim batch");
                    sleep(EMAIL_POLL_INTERVAL).await;
                    continue;
                }
            };

            if claimed.is_empty() {
                sleep(EMAIL_POLL_INTERVAL).await;
                continue;
            }

            let total = claimed.len();
            info!(claimed = total, "processing batch");

            futures::stream::iter(claimed)
                .for_each_concurrent(EMAIL_MAX_CONCURRENT, |email| {
                    let this = Arc::clone(&this);
                    let mailer = mailer.clone();

                    async move {
                        this.process_one(email, &mailer).await;
                    }
                })
                .await;

            debug!(processed = total, "batch finished");
        }
    }

    #[instrument(skip_all)]
    async fn claim_batch(&self) -> Result<Vec<EmailQueue>, sqlx::Error> {
        sqlx::query_as!(
            EmailQueue,
            r#"
            UPDATE email_queue
            SET scheduled_at = NOW() + ($1 || ' minutes')::INTERVAL
            WHERE id IN (
                SELECT id
                FROM email_queue
                WHERE attempts < $2
                  AND scheduled_at <= NOW()
                ORDER BY scheduled_at
                LIMIT $3
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id, from_address, to_address, cc, bcc, subject,
                      html_content, text_content, attempts, created_at
            "#,
            CLAIM_TTL_MINUTES.to_string(),
            MAX_ATTEMPTS as i64,
            EMAIL_BATCH_SIZE,
        )
        .fetch_all(&self.pool)
        .await
    }

    #[instrument(skip_all, fields(email_id = email.id))]
    async fn process_one(&self, email: EmailQueue, mailer: &AsyncSmtpTransport<Tokio1Executor>) {
        let message = match Self::build_message(&email) {
            Ok(m) => m,
            Err(e) => {
                error!(%e, "failed to build email message, marking as failed");
                self.move_to_log_failed(&email, &e.to_string(), MAX_ATTEMPTS)
                    .await;
                return;
            }
        };

        match mailer.send(message).await {
            Ok(_) => {
                debug!("email sent");
                self.move_to_log_sent(&email).await;
            }
            Err(e) => {
                warn!(%e, "failed to send email");
                let new_attempts = email.attempts + 1;

                if new_attempts >= MAX_ATTEMPTS {
                    self.move_to_log_failed(&email, &e.to_string(), new_attempts)
                        .await;
                } else {
                    self.reschedule(&email, new_attempts, &e.to_string()).await;
                }
            }
        }
    }

    fn build_message(email: &EmailQueue) -> Result<Message, anyhow::Error> {
        let to: To = email.to_address.join(", ").parse::<Mailboxes>()?.into();

        let cc: Cc = if email.cc.is_empty() {
            Mailboxes::new().into()
        } else {
            email.cc.join(", ").parse::<Mailboxes>()?.into()
        };

        let bcc: Bcc = if email.bcc.is_empty() {
            Mailboxes::new().into()
        } else {
            email.bcc.join(", ").parse::<Mailboxes>()?.into()
        };

        let message = Message::builder()
            .mailbox(to)
            .mailbox(cc)
            .mailbox(bcc)
            .from(email.from_address.parse()?)
            .subject(&email.subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(email.text_content.clone()),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(email.html_content.clone()),
                    ),
            )?;

        Ok(message)
    }

    async fn move_to_log_sent(&self, email: &EmailQueue) {
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!(email_id = email.id, %e, "failed to start tx (sent)");
                return;
            }
        };

        let result = async {
            sqlx::query!(
                r#"
                INSERT INTO email_log (
                    from_address, to_address, cc, bcc,
                    subject, status, attempts, created_at
                ) VALUES ($1, $2, $3, $4, $5, 'sent', $6, $7)
                "#,
                email.from_address,
                &email.to_address,
                &email.cc,
                &email.bcc,
                email.subject,
                email.attempts + 1,
                email.created_at,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query!("DELETE FROM email_queue WHERE id = $1", email.id)
                .execute(&mut *tx)
                .await?;

            Ok::<_, sqlx::Error>(())
        }
        .await;

        match result {
            Ok(_) => {
                if let Err(e) = tx.commit().await {
                    error!(email_id = email.id, %e, "failed to commit sent tx");
                }
            }
            Err(e) => {
                error!(email_id = email.id, %e, "failed to move email to log (sent)");
                let _ = tx.rollback().await;
            }
        }
    }

    async fn move_to_log_failed(&self, email: &EmailQueue, err: &str, attempts: i16) {
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!(email_id = email.id, %e, "failed to start tx (failed)");
                return;
            }
        };

        let result = async {
            sqlx::query!(
                r#"
                INSERT INTO email_log (
                    from_address, to_address, cc, bcc,
                    subject, status, error_msg, attempts, created_at
                ) VALUES ($1, $2, $3, $4, $5, 'failed', $6, $7, $8)
                "#,
                email.from_address,
                &email.to_address,
                &email.cc,
                &email.bcc,
                email.subject,
                err,
                attempts,
                email.created_at,
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query!("DELETE FROM email_queue WHERE id = $1", email.id)
                .execute(&mut *tx)
                .await?;

            Ok::<_, sqlx::Error>(())
        }
        .await;

        match result {
            Ok(_) => {
                if let Err(e) = tx.commit().await {
                    error!(email_id = email.id, %e, "failed to commit failed tx");
                }
            }
            Err(e) => {
                error!(email_id = email.id, %e, "failed to move email to log (failed)");
                let _ = tx.rollback().await;
            }
        }
    }

    async fn reschedule(&self, email: &EmailQueue, new_attempts: i16, err: &str) {
        let backoff_mins = 2_i64.pow(new_attempts as u32);

        let res = sqlx::query!(
            r#"
            UPDATE email_queue
            SET attempts = $1,
                last_error = $2,
                scheduled_at = NOW() + ($3 || ' minutes')::INTERVAL
            WHERE id = $4
            "#,
            new_attempts,
            err,
            backoff_mins.to_string(),
            email.id,
        )
        .execute(&self.pool)
        .await;

        if let Err(e) = res {
            error!(email_id = email.id, %e, "failed to reschedule email");
        }
    }
}
