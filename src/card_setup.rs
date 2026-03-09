//! Adaptive Card setup flow types.
//!
//! Provides types for driving setup/onboard workflows via adaptive cards
//! in messaging channels. The actual card rendering uses greentic-qa's
//! `render_card` function; this module adds security (signed tokens)
//! and multi-step orchestration on top.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A setup session that tracks multi-step card-based onboarding.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CardSetupSession {
    /// Unique session ID.
    pub session_id: String,
    /// Bundle being configured.
    pub bundle_path: PathBuf,
    /// Provider being configured.
    pub provider_id: String,
    /// Tenant context.
    pub tenant: String,
    /// Team context.
    #[serde(default)]
    pub team: Option<String>,
    /// Answers collected so far.
    pub answers: HashMap<String, Value>,
    /// Current step index.
    pub current_step: usize,
    /// When this session was created (Unix timestamp).
    pub created_at: u64,
    /// When this session expires (Unix timestamp).
    pub expires_at: u64,
    /// Whether this session has been completed.
    pub completed: bool,
}

impl CardSetupSession {
    /// Create a new session with the given TTL.
    pub fn new(
        bundle_path: PathBuf,
        provider_id: String,
        tenant: String,
        team: Option<String>,
        ttl: Duration,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            session_id: generate_session_id(),
            bundle_path,
            provider_id,
            tenant,
            team,
            answers: HashMap::new(),
            current_step: 0,
            created_at: now,
            expires_at: now + ttl.as_secs(),
            completed: false,
        }
    }

    /// Check whether this session has expired.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now >= self.expires_at
    }

    /// Merge new answers into the session.
    pub fn merge_answers(&mut self, new_answers: &Value) {
        if let Some(obj) = new_answers.as_object() {
            for (key, value) in obj {
                if !value.is_null() {
                    self.answers.insert(key.clone(), value.clone());
                }
            }
        }
    }

    /// Get collected answers as a JSON Value.
    pub fn answers_as_value(&self) -> Value {
        serde_json::to_value(&self.answers).unwrap_or(Value::Object(Default::default()))
    }
}

/// Configuration for setup link generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupLinkConfig {
    /// Base URL for the setup endpoint.
    pub base_url: String,
    /// Default TTL for setup sessions.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,
    /// Signing key for setup tokens (hex-encoded).
    #[serde(default)]
    pub signing_key: Option<String>,
}

fn default_ttl_secs() -> u64 {
    1800 // 30 minutes
}

impl SetupLinkConfig {
    /// Generate a setup URL for the given session.
    ///
    /// Format: `{base_url}/setup?session={session_id}&token={token}`
    pub fn generate_url(&self, session: &CardSetupSession) -> String {
        // Token is the session_id itself for now.
        // In production, this should be a signed JWT.
        format!(
            "{}/setup?session={}&provider={}",
            self.base_url.trim_end_matches('/'),
            session.session_id,
            session.provider_id,
        )
    }
}

/// Result of processing a card setup submission.
#[derive(Clone, Debug, Serialize)]
pub struct CardSetupResult {
    /// Whether setup is complete (all steps answered).
    pub complete: bool,
    /// The next card to render (if not complete).
    pub next_card: Option<Value>,
    /// Warnings from the setup process.
    pub warnings: Vec<String>,
    /// Keys that were persisted.
    pub persisted_keys: Vec<String>,
}

fn generate_session_id() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("setup-{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_not_expired_within_ttl() {
        let session = CardSetupSession::new(
            PathBuf::from("/bundle"),
            "telegram".into(),
            "demo".into(),
            None,
            Duration::from_secs(3600),
        );
        assert!(!session.is_expired());
        assert!(!session.completed);
        assert!(session.session_id.starts_with("setup-"));
    }

    #[test]
    fn session_expired_with_zero_ttl() {
        let session = CardSetupSession::new(
            PathBuf::from("/bundle"),
            "telegram".into(),
            "demo".into(),
            None,
            Duration::from_secs(0),
        );
        assert!(session.is_expired());
    }

    #[test]
    fn merge_answers_accumulates() {
        let mut session = CardSetupSession::new(
            PathBuf::from("/bundle"),
            "telegram".into(),
            "demo".into(),
            None,
            Duration::from_secs(3600),
        );
        session.merge_answers(&serde_json::json!({"bot_token": "abc"}));
        session.merge_answers(&serde_json::json!({"public_url": "https://example.com"}));
        assert_eq!(session.answers.len(), 2);
        assert_eq!(
            session.answers.get("bot_token"),
            Some(&Value::String("abc".into()))
        );
    }

    #[test]
    fn null_answers_not_merged() {
        let mut session = CardSetupSession::new(
            PathBuf::from("/bundle"),
            "telegram".into(),
            "demo".into(),
            None,
            Duration::from_secs(3600),
        );
        session.merge_answers(&serde_json::json!({"key": null}));
        assert!(session.answers.is_empty());
    }

    #[test]
    fn setup_link_generation() {
        let config = SetupLinkConfig {
            base_url: "https://operator.example.com".into(),
            ttl_secs: 1800,
            signing_key: None,
        };
        let session = CardSetupSession::new(
            PathBuf::from("/bundle"),
            "telegram".into(),
            "demo".into(),
            None,
            Duration::from_secs(1800),
        );
        let url = config.generate_url(&session);
        assert!(url.starts_with("https://operator.example.com/setup?session="));
        assert!(url.contains("provider=telegram"));
    }
}
