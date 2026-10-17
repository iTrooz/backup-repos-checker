use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub encryption: Encryption,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default = "default_subject")]
    pub subject: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encryption {
    #[default]
    Starttls,
    Tls,
}

fn default_subject() -> String {
    "Backup repository alerts".to_owned()
}

impl EmailConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.smtp_host.trim().is_empty(),
            "email.smtp_host cannot be empty"
        );
        ensure!(
            !self.smtp_host.contains(['/', '\r', '\n']),
            "email.smtp_host must be a hostname, not a URL"
        );
        ensure!(
            self.smtp_port != Some(0),
            "email.smtp_port must be positive"
        );
        self.message(String::new())?;
        Ok(())
    }

    pub fn message(&self, body: String) -> Result<Message> {
        ensure!(
            !self.to.is_empty(),
            "email.to must contain at least one recipient"
        );
        let mut message = Message::builder()
            .from(self.from.parse().context("invalid email.from address")?)
            .subject(&self.subject);
        for recipient in &self.to {
            message = message.to(recipient.parse().context("invalid email.to address")?);
        }
        message.body(body).context("cannot build report email")
    }

    pub async fn send(&self, message: Message) -> Result<()> {
        let (builder, default_port) = match self.encryption {
            Encryption::Starttls => (
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.smtp_host)?,
                587,
            ),
            Encryption::Tls => (
                AsyncSmtpTransport::<Tokio1Executor>::relay(&self.smtp_host)?,
                465,
            ),
        };
        let mut builder = builder
            .port(self.smtp_port.unwrap_or(default_port))
            .timeout(Some(Duration::from_secs(30)));
        match (
            std::env::var("SMTP_USERNAME"),
            std::env::var("SMTP_PASSWORD"),
        ) {
            (Ok(username), Ok(password)) if !username.is_empty() && !password.is_empty() => {
                builder = builder.credentials(Credentials::new(username, password));
            }
            (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {}
            _ => bail!(
                "set both SMTP_USERNAME and SMTP_PASSWORD to nonempty values, or neither for unauthenticated SMTP"
            ),
        }
        builder
            .build()
            .send(message)
            .await
            .context("cannot send report email")?;
        Ok(())
    }
}

pub fn body(issues: &[String], max_age_days: u32) -> String {
    format!(
        "Backup repository report\nFreshness threshold: {max_age_days} days\n{} problematic entries:\n\n{}\n",
        issues.len(),
        issues.join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EmailConfig {
        toml::from_str("smtp_host = 'smtp.example.com'\nfrom = 'backups@example.com'\nto = ['ops@example.com', 'other@example.com']").unwrap()
    }

    #[test]
    fn builds_plain_text_email_with_all_recipients() {
        let config = config();
        config.validate().unwrap();
        let body = body(&["[MISSING] s3://bucket/repo".to_owned()], 7);
        let message = config.message(body).unwrap();
        assert_eq!(message.envelope().to().len(), 2);
        let formatted = String::from_utf8(message.formatted()).unwrap();
        assert!(formatted.contains("Subject: Backup repository alerts"));
        assert!(formatted.contains("[MISSING] s3://bucket/repo"));
        assert!(formatted.contains("Freshness threshold: 7 days"));
    }

    #[test]
    fn rejects_invalid_mail_configuration() {
        let mut config = config();
        config.to.clear();
        assert!(config.validate().is_err());
        config.to = vec!["not an address".into()];
        assert!(config.validate().is_err());
        config.to = vec!["ops@example.com".into()];
        config.smtp_port = Some(0);
        assert!(config.validate().is_err());
    }
}
