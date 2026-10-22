use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};

use crate::{
    config::{Config, SmtpConfig},
    error::KatError,
};

#[derive(Debug, Clone, Copy)]
pub enum MailKind {
    PasswordReset,
}

#[derive(Clone)]
pub struct Mail {
    pub kind: MailKind,
    pub to: String,
    pub subject: String,
    pub body: String,
}

impl fmt::Debug for Mail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mail")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, mail: &Mail) -> Result<(), KatError>;
}

pub fn from_config(config: &Config) -> Arc<dyn Mailer> {
    match &config.smtp {
        Some(smtp) => Arc::new(SmtpMailer {
            config: smtp.clone(),
        }),
        None if config.demo => Arc::new(ConsoleMailer),
        None => Arc::new(DisabledMailer),
    }
}

pub struct DisabledMailer;

#[async_trait]
impl Mailer for DisabledMailer {
    async fn send(&self, mail: &Mail) -> Result<(), KatError> {
        tracing::info!(kind = ?mail.kind, outcome = "smtp_unconfigured", "mail_not_sent");
        Ok(())
    }
}

pub struct ConsoleMailer;

#[async_trait]
impl Mailer for ConsoleMailer {
    async fn send(&self, mail: &Mail) -> Result<(), KatError> {
        tracing::warn!(kind = ?mail.kind, to = %mail.to, subject = %mail.subject, body = %mail.body, "demo only: console mail");
        Ok(())
    }
}

pub struct SmtpMailer {
    config: SmtpConfig,
}

impl SmtpMailer {
    async fn deliver(&self, mail: &Mail) -> Result<(), KatError> {
        let port = self.config.port.unwrap_or(587);
        let builder = if port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.config.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.config.host)
        };
        let mut builder = builder
            .map_err(|_| KatError::MailDeliveryFailed)?
            .port(port)
            .timeout(Some(Duration::from_secs(10)));
        if let Some(username) = &self.config.username {
            builder = builder.credentials(Credentials::new(
                username.clone(),
                self.config.password.clone().unwrap_or_default(),
            ));
        }
        let message = Message::builder()
            .from(
                self.config
                    .from
                    .as_deref()
                    .unwrap_or("noreply@katwork.local")
                    .parse()
                    .map_err(|_| KatError::MailDeliveryFailed)?,
            )
            .to(mail.to.parse().map_err(|_| KatError::MailDeliveryFailed)?)
            .subject(&mail.subject)
            .body(mail.body.clone())
            .map_err(|_| KatError::MailDeliveryFailed)?;
        builder
            .build()
            .send(message)
            .await
            .map_err(|_| KatError::MailDeliveryFailed)?;
        Ok(())
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, mail: &Mail) -> Result<(), KatError> {
        let result = self.deliver(mail).await;
        tracing::info!(kind = ?mail.kind, outcome = if result.is_ok() { "sent" } else { "failed" }, "mail_delivery");
        result
    }
}

#[derive(Default)]
pub struct CapturingMailer {
    sent: Mutex<Vec<Mail>>,
}

impl CapturingMailer {
    pub fn sent(&self) -> Vec<Mail> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl Mailer for CapturingMailer {
    async fn send(&self, mail: &Mail) -> Result<(), KatError> {
        self.sent.lock().unwrap().push(mail.clone());
        Ok(())
    }
}
