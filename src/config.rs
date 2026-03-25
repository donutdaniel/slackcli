use std::collections::BTreeMap;
use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use directories::ProjectDirs;
use reqwest::Url;
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "slackcli";
pub const LEGACY_APP_NAME: &str = "slack-cli";
pub const DEFAULT_API_BASE_URL: &str = "https://slack.com/api";
pub const SLACK_API_BASE_URL_ENV: &str = "SLACK_API_BASE_URL";
pub const UNSAFE_LOCAL_API_BASE_URL_ENV: &str = "SLACKCLI_UNSAFE_ALLOW_LOCAL_API_BASE_URL";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub credentials_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        if let Ok(raw) =
            env::var("SLACKCLI_CONFIG_DIR").or_else(|_| env::var("SLACK_CLI_CONFIG_DIR"))
        {
            return Ok(Self::from_base(PathBuf::from(raw)));
        }

        let new_dirs = project_config_dir(APP_NAME)?;
        let legacy_dirs = project_config_dir(LEGACY_APP_NAME)?;

        if !new_dirs.has_local_state() && legacy_dirs.has_local_state() {
            return Ok(legacy_dirs);
        }

        Ok(new_dirs)
    }

    pub fn from_base(base: PathBuf) -> Self {
        let config_file = base.join("config.toml");
        let credentials_file = base.join("credentials.toml");
        Self {
            config_dir: base,
            config_file,
            credentials_file,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir).with_context(|| {
            format!(
                "failed to create config directory at {}",
                self.config_dir.display()
            )
        })
    }

    fn has_local_state(&self) -> bool {
        self.config_file.exists() || self.credentials_file.exists()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigFile {
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub auth_type: AuthType,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub enterprise_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    User,
    Bot,
    Unknown,
}

impl std::fmt::Display for AuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Bot => write!(f, "bot"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredSecret {
    Token { token: String },
}

impl StoredSecret {
    pub fn access_token(&self) -> &str {
        let Self::Token { token } = self;
        token
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    PersistedProfile,
    Environment,
}

#[derive(Debug, Clone)]
pub struct RuntimeSession {
    pub profile_name: Option<String>,
    pub secret: StoredSecret,
    pub source: SessionSource,
}

trait SecretStore: Send + Sync {
    fn write_secret(&self, profile_name: &str, secret: &StoredSecret) -> Result<()>;
    fn read_secret(&self, profile_name: &str) -> Result<StoredSecret>;
    fn delete_secret(&self, profile_name: &str) -> Result<()>;
    fn has_secret(&self, profile_name: &str) -> bool;
}

struct FileSecretStore {
    credentials_file: PathBuf,
}

impl FileSecretStore {
    fn new(credentials_file: PathBuf) -> Self {
        Self { credentials_file }
    }

    fn load_credentials(&self) -> Result<CredentialsFile> {
        if !self.credentials_file.exists() {
            return Ok(CredentialsFile::default());
        }

        let raw = fs::read_to_string(&self.credentials_file).with_context(|| {
            format!(
                "failed to read credentials file {}",
                self.credentials_file.display()
            )
        })?;

        toml::from_str(&raw).with_context(|| {
            format!(
                "failed to parse credentials file {}",
                self.credentials_file.display()
            )
        })
    }

    fn save_credentials(&self, credentials: &CredentialsFile) -> Result<()> {
        if let Some(parent) = self.credentials_file.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory at {}", parent.display())
            })?;
        }

        let raw = toml::to_string_pretty(credentials).context("failed to serialize credentials")?;
        let temp_path = self.credentials_file.with_extension("toml.tmp");
        fs::write(&temp_path, raw).with_context(|| {
            format!(
                "failed to write temporary credentials file {}",
                temp_path.display()
            )
        })?;
        set_private_permissions(&temp_path)?;
        fs::rename(&temp_path, &self.credentials_file).with_context(|| {
            format!(
                "failed to replace credentials file {}",
                self.credentials_file.display()
            )
        })?;
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn write_secret(&self, profile_name: &str, secret: &StoredSecret) -> Result<()> {
        let mut credentials = self.load_credentials()?;
        credentials
            .profiles
            .insert(profile_name.to_string(), secret.clone());
        self.save_credentials(&credentials)
    }

    fn read_secret(&self, profile_name: &str) -> Result<StoredSecret> {
        let credentials = self.load_credentials()?;
        credentials.profiles.get(profile_name).cloned().ok_or_else(|| {
            anyhow!(
                "no persisted credentials found for profile `{profile_name}`; run `slackcli auth login ...` again"
            )
        })
    }

    fn delete_secret(&self, profile_name: &str) -> Result<()> {
        let mut credentials = self.load_credentials()?;
        credentials.profiles.remove(profile_name);
        self.save_credentials(&credentials)
    }

    fn has_secret(&self, profile_name: &str) -> bool {
        self.load_credentials()
            .map(|credentials| credentials.profiles.contains_key(profile_name))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CredentialsFile {
    #[serde(default)]
    profiles: BTreeMap<String, StoredSecret>,
}

#[cfg(test)]
struct InMemorySecretStore {
    secrets: std::sync::Mutex<BTreeMap<String, StoredSecret>>,
}

#[cfg(test)]
impl Default for InMemorySecretStore {
    fn default() -> Self {
        Self {
            secrets: std::sync::Mutex::new(BTreeMap::new()),
        }
    }
}

#[cfg(test)]
impl SecretStore for InMemorySecretStore {
    fn write_secret(&self, profile_name: &str, secret: &StoredSecret) -> Result<()> {
        let mut secrets = self
            .secrets
            .lock()
            .map_err(|_| anyhow!("in-memory test secret store is poisoned"))?;
        secrets.insert(profile_name.to_string(), secret.clone());
        Ok(())
    }

    fn read_secret(&self, profile_name: &str) -> Result<StoredSecret> {
        let secrets = self
            .secrets
            .lock()
            .map_err(|_| anyhow!("in-memory test secret store is poisoned"))?;
        secrets.get(profile_name).cloned().ok_or_else(|| {
            anyhow!(
                "no persisted credentials found for profile `{profile_name}`; run `slackcli auth login ...` again"
            )
        })
    }

    fn delete_secret(&self, profile_name: &str) -> Result<()> {
        let mut secrets = self
            .secrets
            .lock()
            .map_err(|_| anyhow!("in-memory test secret store is poisoned"))?;
        secrets.remove(profile_name);
        Ok(())
    }

    fn has_secret(&self, profile_name: &str) -> bool {
        self.secrets
            .lock()
            .map(|secrets| secrets.contains_key(profile_name))
            .unwrap_or(false)
    }
}

pub struct ConfigStore {
    paths: AppPaths,
    config: ConfigFile,
    secret_store: Arc<dyn SecretStore>,
}

impl ConfigStore {
    pub fn load() -> Result<Self> {
        let paths = AppPaths::discover()?;
        Self::with_secret_store(
            paths.clone(),
            Arc::new(FileSecretStore::new(paths.credentials_file.clone())),
        )
    }

    fn with_secret_store(paths: AppPaths, secret_store: Arc<dyn SecretStore>) -> Result<Self> {
        let config = if paths.config_file.exists() {
            let raw = fs::read_to_string(&paths.config_file).with_context(|| {
                format!("failed to read config file {}", paths.config_file.display())
            })?;
            toml::from_str(&raw).with_context(|| {
                format!(
                    "failed to parse config file {}",
                    paths.config_file.display()
                )
            })?
        } else {
            ConfigFile::default()
        };

        Ok(Self {
            paths,
            config,
            secret_store,
        })
    }

    #[cfg(test)]
    fn with_paths_and_secret_store(
        paths: AppPaths,
        secret_store: Arc<dyn SecretStore>,
    ) -> Result<Self> {
        Self::with_secret_store(paths, secret_store)
    }

    pub fn api_base_url(&self) -> Result<String> {
        let override_value = env::var(SLACK_API_BASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        resolve_api_base_url(
            override_value.as_deref(),
            allow_local_api_base_url_override(),
        )
    }

    pub fn save(&self) -> Result<()> {
        self.paths.ensure()?;
        let raw = toml::to_string_pretty(&self.config).context("failed to serialize config")?;
        fs::write(&self.paths.config_file, raw).with_context(|| {
            format!(
                "failed to write config file {}",
                self.paths.config_file.display()
            )
        })
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn active_profile(&self) -> Option<&str> {
        self.config.active_profile.as_deref()
    }

    pub fn profiles(&self) -> &BTreeMap<String, ProfileMeta> {
        &self.config.profiles
    }

    pub fn get_profile(&self, name: &str) -> Option<&ProfileMeta> {
        self.config.profiles.get(name)
    }

    pub fn put_profile(
        &mut self,
        name: String,
        meta: ProfileMeta,
        secret: &StoredSecret,
    ) -> Result<()> {
        self.secret_store.write_secret(&name, secret)?;
        self.config.profiles.insert(name.clone(), meta);
        self.config.active_profile = Some(name);
        self.save()
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<()> {
        self.config.profiles.remove(name);
        self.secret_store.delete_secret(name)?;

        if self.config.active_profile.as_deref() == Some(name) {
            self.config.active_profile = self
                .config
                .profiles
                .keys()
                .next()
                .map(std::string::ToString::to_string);
        }

        self.save()
    }

    pub fn set_active_profile(&mut self, name: &str) -> Result<()> {
        if !self.config.profiles.contains_key(name) {
            bail!("profile `{name}` does not exist");
        }

        self.config.active_profile = Some(name.to_string());
        self.save()
    }

    pub fn has_persisted_secret(&self, name: &str) -> bool {
        self.secret_store.has_secret(name)
    }

    pub fn resolve_session(&self, explicit_profile: Option<&str>) -> Result<RuntimeSession> {
        let profile = explicit_profile
            .map(str::to_string)
            .or_else(|| self.config.active_profile.clone());

        if let Some(token) = runtime_token_override() {
            return Ok(RuntimeSession {
                profile_name: profile,
                secret: StoredSecret::Token { token },
                source: SessionSource::Environment,
            });
        }

        let profile = profile.ok_or_else(|| {
            anyhow!("no active profile configured; run `slackcli auth login` or set `SLACK_TOKEN`")
        })?;

        let secret = self.secret_store.read_secret(&profile)?;
        Ok(RuntimeSession {
            profile_name: Some(profile),
            secret,
            source: SessionSource::PersistedProfile,
        })
    }
}

fn project_config_dir(app_name: &str) -> Result<AppPaths> {
    let dirs = ProjectDirs::from("com", "slack", app_name)
        .context("could not determine a platform config directory")?;
    Ok(AppPaths::from_base(dirs.config_dir().to_path_buf()))
}

fn runtime_token_override() -> Option<String> {
    env::var("SLACK_TOKEN")
        .ok()
        .or_else(|| env::var("SLACKCLI_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn allow_local_api_base_url_override() -> bool {
    env_var_truthy(UNSAFE_LOCAL_API_BASE_URL_ENV)
}

pub fn is_local_host(host: &str) -> bool {
    matches!(
        host.trim_matches(['[', ']']),
        "localhost" | "127.0.0.1" | "::1"
    )
}

pub fn is_slack_host(host: &str) -> bool {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    normalized == "slack.com"
        || normalized.ends_with(".slack.com")
        || normalized == "slack-gov.com"
        || normalized.ends_with(".slack-gov.com")
}

fn resolve_api_base_url(
    override_value: Option<&str>,
    allow_local_override: bool,
) -> Result<String> {
    let Some(raw) = override_value else {
        return Ok(DEFAULT_API_BASE_URL.to_string());
    };

    let url = Url::parse(raw).with_context(|| {
        format!("invalid {SLACK_API_BASE_URL_ENV} override: must be an absolute URL")
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("invalid {SLACK_API_BASE_URL_ENV} override: credentials in URLs are not allowed")
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!(
            "invalid {SLACK_API_BASE_URL_ENV} override: query strings and fragments are not allowed"
        )
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("invalid {SLACK_API_BASE_URL_ENV} override: missing host"))?;

    if is_local_host(host) {
        if !allow_local_override {
            bail!(
                "{SLACK_API_BASE_URL_ENV} may only target localhost when {UNSAFE_LOCAL_API_BASE_URL_ENV}=1"
            )
        }
        if !matches!(url.scheme(), "http" | "https") {
            bail!(
                "invalid {SLACK_API_BASE_URL_ENV} override: localhost overrides must use http or https"
            )
        }
        return Ok(raw.trim_end_matches('/').to_string());
    }

    if url.scheme() != "https" {
        bail!("invalid {SLACK_API_BASE_URL_ENV} override: Slack API endpoints must use https")
    }
    if !is_slack_host(host) {
        bail!("invalid {SLACK_API_BASE_URL_ENV} override: host must be slack.com or slack-gov.com")
    }
    if let Some(port) = url.port()
        && port != 443
    {
        bail!("invalid {SLACK_API_BASE_URL_ENV} override: Slack API endpoints must use port 443")
    }

    let normalized_path = url.path().trim_end_matches('/');
    let final_url = if normalized_path.is_empty() {
        let mut normalized = url;
        normalized.set_path("/api");
        normalized
    } else {
        if normalized_path != "/api" {
            bail!(
                "invalid {SLACK_API_BASE_URL_ENV} override: Slack API endpoints must end with /api"
            )
        }
        url
    };

    Ok(final_url.to_string().trim_end_matches('/').to_string())
}

fn env_var_truthy(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to set private permissions on {}", path.display()))?;
    }
    Ok(())
}

pub fn slugify_profile_name(raw: &str) -> String {
    let mut output = String::new();
    let mut last_dash = false;

    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !output.is_empty() {
            output.push('-');
            last_dash = true;
        }
    }

    output.trim_matches('-').to_string()
}

pub fn read_text_file(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn slugifies_profile_names() {
        assert_eq!(slugify_profile_name("My Workspace"), "my-workspace");
        assert_eq!(slugify_profile_name("  QA / Eng  "), "qa-eng");
    }

    #[test]
    fn defaults_to_slack_api_base_url() -> Result<()> {
        assert_eq!(resolve_api_base_url(None, false)?, DEFAULT_API_BASE_URL);
        Ok(())
    }

    #[test]
    fn accepts_slack_owned_https_base_url_without_explicit_api_suffix() -> Result<()> {
        assert_eq!(
            resolve_api_base_url(Some("https://slack-gov.com"), false)?,
            "https://slack-gov.com/api"
        );
        Ok(())
    }

    #[test]
    fn rejects_non_slack_api_base_url() {
        let error = resolve_api_base_url(Some("https://example.com/api"), false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("host must be slack.com or slack-gov.com")
        );
    }

    #[test]
    fn rejects_local_api_override_without_explicit_opt_in() {
        let error = resolve_api_base_url(Some("http://127.0.0.1:8080"), false).unwrap_err();
        assert!(error.to_string().contains("may only target localhost when"));
    }

    #[test]
    fn accepts_local_api_override_with_explicit_opt_in() -> Result<()> {
        assert_eq!(
            resolve_api_base_url(Some("http://127.0.0.1:8080/mock"), true)?,
            "http://127.0.0.1:8080/mock"
        );
        Ok(())
    }

    #[test]
    fn config_round_trip() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = AppPaths::from_base(temp.path().to_path_buf());
        let secret_store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        let mut store = ConfigStore::with_paths_and_secret_store(paths, secret_store.clone())?;

        let meta = ProfileMeta {
            auth_type: AuthType::User,
            team_id: Some("T123".into()),
            team_name: Some("Workspace".into()),
            enterprise_id: None,
            url: Some("https://example.slack.com".into()),
            user_id: Some("U123".into()),
            user_name: Some("alice".into()),
            bot_id: None,
        };

        store.put_profile(
            "workspace".into(),
            meta.clone(),
            &StoredSecret::Token {
                token: "xoxp-test".into(),
            },
        )?;

        let loaded = ConfigStore::with_paths_and_secret_store(
            AppPaths::from_base(temp.path().to_path_buf()),
            secret_store,
        )?;
        let profile = loaded.get_profile("workspace").context("missing profile")?;
        let session = loaded.resolve_session(None)?;

        assert_eq!(profile.team_name.as_deref(), Some("Workspace"));
        assert_eq!(loaded.active_profile(), Some("workspace"));
        assert_eq!(session.profile_name.as_deref(), Some("workspace"));
        assert_eq!(session.secret.access_token(), "xoxp-test");
        assert_eq!(session.source, SessionSource::PersistedProfile);

        Ok(())
    }

    #[test]
    fn set_active_profile_rejects_unknown_profile() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = AppPaths::from_base(temp.path().to_path_buf());
        let secret_store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        let mut store = ConfigStore::with_paths_and_secret_store(paths, secret_store)?;

        store.put_profile(
            "workspace".into(),
            ProfileMeta {
                auth_type: AuthType::User,
                team_id: Some("T123".into()),
                team_name: Some("Workspace".into()),
                enterprise_id: None,
                url: None,
                user_id: Some("U123".into()),
                user_name: Some("alice".into()),
                bot_id: None,
            },
            &StoredSecret::Token {
                token: "xoxp-test".into(),
            },
        )?;

        let error = store.set_active_profile("missing").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("profile `missing` does not exist")
        );
        assert_eq!(store.active_profile(), Some("workspace"));

        Ok(())
    }

    #[test]
    fn remove_active_profile_promotes_next_profile_and_deletes_secret() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = AppPaths::from_base(temp.path().to_path_buf());
        let secret_store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        let mut store =
            ConfigStore::with_paths_and_secret_store(paths.clone(), secret_store.clone())?;

        let alpha = ProfileMeta {
            auth_type: AuthType::User,
            team_id: Some("T123".into()),
            team_name: Some("Alpha".into()),
            enterprise_id: None,
            url: None,
            user_id: Some("U123".into()),
            user_name: Some("alice".into()),
            bot_id: None,
        };
        let beta = ProfileMeta {
            auth_type: AuthType::User,
            team_id: Some("T456".into()),
            team_name: Some("Beta".into()),
            enterprise_id: None,
            url: None,
            user_id: Some("U456".into()),
            user_name: Some("bob".into()),
            bot_id: None,
        };

        store.put_profile(
            "alpha".into(),
            alpha,
            &StoredSecret::Token {
                token: "xoxp-alpha".into(),
            },
        )?;
        store.put_profile(
            "beta".into(),
            beta,
            &StoredSecret::Token {
                token: "xoxp-beta".into(),
            },
        )?;
        store.set_active_profile("alpha")?;

        store.remove_profile("alpha")?;

        assert_eq!(store.active_profile(), Some("beta"));
        assert!(store.get_profile("alpha").is_none());
        assert!(!store.has_persisted_secret("alpha"));
        assert!(store.has_persisted_secret("beta"));

        let reloaded = ConfigStore::with_paths_and_secret_store(paths, secret_store)?;
        assert_eq!(reloaded.active_profile(), Some("beta"));
        assert!(reloaded.get_profile("alpha").is_none());

        Ok(())
    }

    #[test]
    fn resolve_session_prefers_explicit_profile() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = AppPaths::from_base(temp.path().to_path_buf());
        let secret_store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        let mut store = ConfigStore::with_paths_and_secret_store(paths, secret_store)?;

        store.put_profile(
            "alpha".into(),
            ProfileMeta {
                auth_type: AuthType::User,
                team_id: Some("T123".into()),
                team_name: Some("Alpha".into()),
                enterprise_id: None,
                url: None,
                user_id: Some("U123".into()),
                user_name: Some("alice".into()),
                bot_id: None,
            },
            &StoredSecret::Token {
                token: "xoxp-alpha".into(),
            },
        )?;
        store.put_profile(
            "beta".into(),
            ProfileMeta {
                auth_type: AuthType::User,
                team_id: Some("T456".into()),
                team_name: Some("Beta".into()),
                enterprise_id: None,
                url: None,
                user_id: Some("U456".into()),
                user_name: Some("bob".into()),
                bot_id: None,
            },
            &StoredSecret::Token {
                token: "xoxp-beta".into(),
            },
        )?;
        store.set_active_profile("alpha")?;

        let session = store.resolve_session(Some("beta"))?;

        assert_eq!(session.profile_name.as_deref(), Some("beta"));
        assert_eq!(session.secret.access_token(), "xoxp-beta");
        assert_eq!(session.source, SessionSource::PersistedProfile);

        Ok(())
    }
}
