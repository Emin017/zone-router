use crate::config::{Backend, Config, ConfigError};
use crate::stats::StatsCollector;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Config,
    pub config_path: PathBuf,
    pub active_index: usize,
    pub local_token: String,
    pub stats: StatsCollector,
    pub shutdown: bool,
}

impl AppState {
    pub fn new(config: Config, config_path: PathBuf) -> Result<Self, ConfigError> {
        let active_index = config.initial_active_index();
        let local_token = if config.proxy.local_token.is_empty() {
            generate_token()
        } else {
            config.proxy.local_token.clone()
        };
        let mut state = Self {
            config,
            config_path,
            active_index,
            local_token: local_token.clone(),
            stats: StatsCollector::default(),
            shutdown: false,
        };
        if state.config.proxy.local_token.is_empty() {
            state.config.proxy.local_token = local_token;
            state.persist_config()?;
        }
        Ok(state)
    }

    pub fn active_backend(&self) -> Option<&Backend> {
        self.config.backends.get(self.active_index)
    }

    pub fn switch_backend(&mut self, index: usize) -> bool {
        if index < self.config.backends.len() && index != self.active_index {
            self.active_index = index;
            let _ = self.persist_config();
            true
        } else {
            false
        }
    }

    pub fn persist_config(&self) -> Result<(), ConfigError> {
        let mut config = self.config.clone();
        config.proxy.local_token = self.local_token.clone();
        config.backends.iter_mut().enumerate().for_each(|(i, b)| {
            b.active = i == self.active_index;
        });
        config.save(&self.config_path)
    }

    pub fn add_backend(&mut self, backend: Backend) {
        self.config.backends.push(backend);
        let _ = self.persist_config();
    }

    pub fn remove_backend(&mut self, index: usize) -> bool {
        if index >= self.config.backends.len() {
            return false;
        }
        self.config.backends.remove(index);
        if self.config.backends.is_empty() {
            self.active_index = 0;
        } else if index < self.active_index {
            self.active_index -= 1;
        } else if index == self.active_index && self.active_index >= self.config.backends.len() {
            self.active_index = self.config.backends.len() - 1;
        }
        let _ = self.persist_config();
        true
    }

    pub fn update_backend(
        &mut self,
        index: usize,
        name: String,
        url: String,
        token: String,
    ) -> bool {
        if let Some(b) = self.config.backends.get_mut(index) {
            b.name = name;
            b.url = url;
            b.token = token;
            let _ = self.persist_config();
            true
        } else {
            false
        }
    }
}

fn generate_token() -> String {
    format!("sk-local-{}", uuid::Uuid::new_v4().as_simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyConfig;

    fn test_config() -> Config {
        Config {
            proxy: ProxyConfig {
                listen: "127.0.0.1:8080".into(),
                local_token: String::new(),
            },
            backends: vec![
                Backend {
                    name: "a".into(),
                    url: "http://a".into(),
                    token: "ta".into(),
                    active: true,
                },
                Backend {
                    name: "b".into(),
                    url: "http://b".into(),
                    token: "tb".into(),
                    active: false,
                },
            ],
        }
    }

    #[test]
    fn new_generates_token_when_empty() {
        let state = AppState::new(test_config(), PathBuf::from("/tmp/test.toml")).unwrap();
        assert!(state.local_token.starts_with("sk-local-"));
    }

    #[test]
    fn new_uses_configured_token() {
        let mut config = test_config();
        config.proxy.local_token = "my-token".into();
        let state = AppState::new(config, PathBuf::from("/tmp/test.toml")).unwrap();
        assert_eq!(state.local_token, "my-token");
    }

    #[test]
    fn active_backend_returns_correct() {
        let state = AppState::new(test_config(), PathBuf::from("/tmp/test.toml")).unwrap();
        assert_eq!(state.active_backend().unwrap().name, "a");
    }

    #[test]
    fn switch_backend_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut state = AppState::new(test_config(), path).unwrap();
        assert!(state.switch_backend(1));
        assert_eq!(state.active_index, 1);
    }

    #[test]
    fn switch_backend_invalid_index() {
        let mut state = AppState::new(test_config(), PathBuf::from("/tmp/test.toml")).unwrap();
        assert!(!state.switch_backend(99));
        assert_eq!(state.active_index, 0);
    }

    #[test]
    fn switch_backend_same_index() {
        let mut state = AppState::new(test_config(), PathBuf::from("/tmp/test.toml")).unwrap();
        assert!(!state.switch_backend(0));
    }

    #[test]
    fn remove_backend_adjusts_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut state = AppState::new(test_config(), path).unwrap();
        state.active_index = 1;
        state.remove_backend(1);
        assert_eq!(state.active_index, 0);
    }

    #[test]
    fn remove_backend_before_active_decrements_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = test_config();
        config.backends.push(Backend {
            name: "c".into(),
            url: "http://c".into(),
            token: "tc".into(),
            active: false,
        });
        let mut state = AppState::new(config, path).unwrap();
        state.active_index = 1;
        state.remove_backend(0);
        assert_eq!(state.active_index, 0);
        assert_eq!(state.active_backend().unwrap().name, "b");
    }

    #[test]
    fn remove_backend_after_active_keeps_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = test_config();
        config.backends.push(Backend {
            name: "c".into(),
            url: "http://c".into(),
            token: "tc".into(),
            active: false,
        });
        let mut state = AppState::new(config, path).unwrap();
        state.active_index = 0;
        state.remove_backend(2);
        assert_eq!(state.active_index, 0);
        assert_eq!(state.active_backend().unwrap().name, "a");
    }

    #[test]
    fn generated_token_persisted_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let state = AppState::new(test_config(), path.clone()).unwrap();
        assert!(state.local_token.starts_with("sk-local-"));
        assert_eq!(state.config.proxy.local_token, state.local_token);
        // Verify it was saved to disk
        let loaded = Config::load_or_create(&path).unwrap();
        assert_eq!(loaded.proxy.local_token, state.local_token);
    }
}
