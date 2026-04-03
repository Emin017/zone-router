use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub enum AuthType {
    #[default]
    #[serde(rename = "api-key")]
    ApiKey,
    #[serde(rename = "bearer")]
    Bearer,
}

impl fmt::Display for AuthType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey => write!(f, "api-key"),
            Self::Bearer => write!(f, "bearer"),
        }
    }
}

impl AuthType {
    pub fn from_input(s: &str) -> Option<Self> {
        match s.trim() {
            "" | "1" | "api-key" => Some(Self::ApiKey),
            "2" | "bearer" => Some(Self::Bearer),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub backends: Vec<Backend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub local_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backend {
    pub name: String,
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub auth_type: AuthType,
}

fn default_listen() -> String {
    "127.0.0.1:8080".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            proxy: ProxyConfig {
                listen: default_listen(),
                local_token: String::new(),
            },
            backends: Vec::new(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("zone-router").join("config.toml"))
    }

    pub fn load_or_create(path: &Path) -> Result<Self, ConfigError> {
        if path.exists() {
            std::fs::read_to_string(path)
                .map_err(ConfigError::Io)
                .and_then(|content| toml::from_str(&content).map_err(ConfigError::Parse))
        } else {
            let config = Self::default();
            config.save(path)?;
            Ok(config)
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        path.parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .map_err(ConfigError::Io)?;
        toml::to_string_pretty(self)
            .map_err(ConfigError::Serialize)
            .and_then(|content| std::fs::write(path, content).map_err(ConfigError::Io))
    }

    pub fn initial_active_index(&self) -> usize {
        self.backends.iter().position(|b| b.active).unwrap_or(0)
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "config I/O error: {e}"),
            Self::Parse(e) => write!(f, "config parse error: {e}"),
            Self::Serialize(e) => write!(f, "config serialize error: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn backend(name: &str, url: &str, token: &str, active: bool) -> Backend {
        Backend {
            name: name.into(),
            url: url.into(),
            token: token.into(),
            active,
            auth_type: AuthType::default(),
        }
    }

    #[test]
    fn parse_valid_config() {
        let toml_str = r#"
[proxy]
listen = "127.0.0.1:9090"
local_token = "test-token"

[[backends]]
name = "main"
url = "https://api.example.com"
token = "sk-xxx"
active = true

[[backends]]
name = "backup"
url = "https://backup.example.com"
token = "sk-yyy"
active = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.proxy.listen, "127.0.0.1:9090");
        assert_eq!(config.proxy.local_token, "test-token");
        assert_eq!(config.backends.len(), 2);
        assert_eq!(config.backends[0].name, "main");
        assert!(config.backends[0].active);
        assert!(!config.backends[1].active);
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[proxy]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.proxy.listen, "127.0.0.1:8080");
        assert!(config.proxy.local_token.is_empty());
        assert!(config.backends.is_empty());
    }

    #[test]
    fn parse_invalid_config() {
        let result = toml::from_str::<Config>("not valid toml {{{}}}");
        assert!(result.is_err());
    }

    #[test]
    fn initial_active_index_finds_first_active() {
        let config = Config {
            proxy: ProxyConfig {
                listen: default_listen(),
                local_token: String::new(),
            },
            backends: vec![
                backend("a", "http://a", "t", false),
                backend("b", "http://b", "t", true),
            ],
        };
        assert_eq!(config.initial_active_index(), 1);
    }

    #[test]
    fn initial_active_index_defaults_to_zero() {
        let config = Config::default();
        assert_eq!(config.initial_active_index(), 0);
    }

    #[test]
    fn load_or_create_creates_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::load_or_create(&path).unwrap();
        assert_eq!(config.proxy.listen, "127.0.0.1:8080");
        assert!(path.exists());
    }

    #[test]
    fn load_or_create_reads_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"[proxy]\nlisten = \"0.0.0.0:3000\"\n")
            .unwrap();
        let config = Config::load_or_create(&path).unwrap();
        assert_eq!(config.proxy.listen, "0.0.0.0:3000");
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let config = Config {
            proxy: ProxyConfig {
                listen: "127.0.0.1:4000".into(),
                local_token: "tok".into(),
            },
            backends: vec![backend("x", "http://x", "t", true)],
        };
        config.save(&path).unwrap();
        let loaded = Config::load_or_create(&path).unwrap();
        assert_eq!(loaded.proxy.listen, "127.0.0.1:4000");
        assert_eq!(loaded.backends.len(), 1);
    }

    #[test]
    fn auth_type_defaults_to_api_key() {
        let toml_str = r#"
[proxy]
listen = "127.0.0.1:8080"

[[backends]]
name = "test"
url = "http://test"
token = "tok"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backends[0].auth_type, AuthType::ApiKey);
    }

    #[test]
    fn auth_type_bearer_parses() {
        let toml_str = r#"
[proxy]

[[backends]]
name = "test"
url = "http://test"
token = "tok"
auth_type = "bearer"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backends[0].auth_type, AuthType::Bearer);
    }

    #[test]
    fn auth_type_api_key_parses() {
        let toml_str = r#"
[proxy]

[[backends]]
name = "test"
url = "http://test"
token = "tok"
auth_type = "api-key"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backends[0].auth_type, AuthType::ApiKey);
    }

    #[test]
    fn auth_type_invalid_fails() {
        let toml_str = r#"
[proxy]

[[backends]]
name = "test"
url = "http://test"
token = "tok"
auth_type = "invalid"
"#;
        assert!(toml::from_str::<Config>(toml_str).is_err());
    }

    #[test]
    fn auth_type_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            proxy: ProxyConfig {
                listen: "127.0.0.1:4000".into(),
                local_token: "tok".into(),
            },
            backends: vec![Backend {
                name: "b".into(),
                url: "http://b".into(),
                token: "t".into(),
                active: false,
                auth_type: AuthType::Bearer,
            }],
        };
        config.save(&path).unwrap();
        let loaded = Config::load_or_create(&path).unwrap();
        assert_eq!(loaded.backends[0].auth_type, AuthType::Bearer);
    }
}
