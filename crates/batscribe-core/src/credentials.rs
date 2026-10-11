//! Provider API keys. Kept in the Linux Secret Service when the user asks to remember them,
//! otherwise in memory for the session. Keys are never written to files, logs or history.

use std::collections::HashMap;
use std::sync::Mutex;

use secret_service::{EncryptionType, SecretService};

const ATTR_APP: (&str, &str) = ("application", "net.batkin.batscribe");

/// A provider whose key is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Deepgram,
    OpenAi,
    Anthropic,
}

impl Provider {
    /// The provider whose key an editing provider ("openai", "anthropic") uses.
    pub fn for_composer(name: &str) -> Option<Self> {
        match name {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }

    pub const ALL: [Provider; 3] = [Provider::Deepgram, Provider::OpenAi, Provider::Anthropic];

    pub fn kind(self) -> &'static str {
        match self {
            Self::Deepgram => "deepgram-api-key",
            Self::OpenAi => "openai-api-key",
            Self::Anthropic => "anthropic-api-key",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Deepgram => "Deepgram API key",
            Self::OpenAi => "OpenAI API key",
            Self::Anthropic => "Anthropic API key",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Deepgram => "deepgram",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// An API key. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: &str) -> Option<Self> {
        let key = key.trim();
        (!key.is_empty()).then(|| Self(key.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(hidden)")
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("Secret Service unavailable: {0}")]
pub struct CredentialError(pub String);

fn err(e: impl std::fmt::Display) -> CredentialError {
    CredentialError(e.to_string())
}

fn attrs(kind: &str) -> HashMap<&str, &str> {
    HashMap::from([ATTR_APP, ("kind", kind)])
}

pub enum CredentialStore {
    SecretService,
    /// For tests and environments without a keyring.
    Memory(Mutex<HashMap<Provider, String>>),
}

impl CredentialStore {
    /// `BATSCRIBE_CREDENTIAL_STORE=memory` keeps keys out of the system keyring.
    pub fn from_env() -> Self {
        match std::env::var("BATSCRIBE_CREDENTIAL_STORE").as_deref() {
            Ok("memory") => Self::Memory(Mutex::default()),
            _ => Self::SecretService,
        }
    }

    pub async fn load(&self, provider: Provider) -> Result<Option<ApiKey>, CredentialError> {
        match self {
            Self::Memory(m) => Ok(m
                .lock()
                .unwrap()
                .get(&provider)
                .and_then(|k| ApiKey::new(k))),
            Self::SecretService => {
                let ss = SecretService::connect(EncryptionType::Dh)
                    .await
                    .map_err(err)?;
                let found = ss.search_items(attrs(provider.kind())).await.map_err(err)?;
                let item = match (found.unlocked.first(), found.locked.first()) {
                    (Some(item), _) => item,
                    (None, Some(item)) => {
                        item.unlock().await.map_err(err)?;
                        item
                    }
                    (None, None) => return Ok(None),
                };
                let secret = item.get_secret().await.map_err(err)?;
                Ok(String::from_utf8(secret)
                    .ok()
                    .as_deref()
                    .and_then(ApiKey::new))
            }
        }
    }

    pub async fn save(&self, provider: Provider, key: &ApiKey) -> Result<(), CredentialError> {
        match self {
            Self::Memory(m) => {
                m.lock().unwrap().insert(provider, key.expose().to_owned());
                Ok(())
            }
            Self::SecretService => {
                let ss = SecretService::connect(EncryptionType::Dh)
                    .await
                    .map_err(err)?;
                let collection = ss.get_default_collection().await.map_err(err)?;
                collection.ensure_unlocked().await.map_err(err)?;
                collection
                    .create_item(
                        provider.label(),
                        attrs(provider.kind()),
                        key.expose().as_bytes(),
                        true,
                        "text/plain",
                    )
                    .await
                    .map_err(err)?;
                Ok(())
            }
        }
    }

    pub async fn delete(&self, provider: Provider) -> Result<(), CredentialError> {
        match self {
            Self::Memory(m) => {
                m.lock().unwrap().remove(&provider);
                Ok(())
            }
            Self::SecretService => {
                let ss = SecretService::connect(EncryptionType::Dh)
                    .await
                    .map_err(err)?;
                let found = ss.search_items(attrs(provider.kind())).await.map_err(err)?;
                for item in found.unlocked.iter().chain(found.locked.iter()) {
                    item.delete().await.map_err(err)?;
                }
                Ok(())
            }
        }
    }
}

/// Keys available to the pipeline: remembered ones plus session-only ones.
#[derive(Default)]
pub struct KeyRing {
    keys: Mutex<HashMap<Provider, (ApiKey, bool)>>,
}

impl KeyRing {
    pub fn get(&self, provider: Provider) -> Option<ApiKey> {
        self.keys
            .lock()
            .unwrap()
            .get(&provider)
            .map(|(k, _)| k.clone())
    }

    /// (has key, persisted)
    pub fn status(&self, provider: Provider) -> (bool, bool) {
        self.keys
            .lock()
            .unwrap()
            .get(&provider)
            .map_or((false, false), |(_, persisted)| (true, *persisted))
    }

    pub fn set(&self, provider: Provider, key: ApiKey, persisted: bool) {
        self.keys.lock().unwrap().insert(provider, (key, persisted));
    }

    pub fn remove(&self, provider: Provider) {
        self.keys.lock().unwrap().remove(&provider);
    }
}
