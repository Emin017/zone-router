use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub fn make_state(
    backends: Vec<(&str, &str, &str)>,
    local_token: &str,
) -> Arc<RwLock<zone_router::state::AppState>> {
    let config = zone_router::config::Config {
        proxy: zone_router::config::ProxyConfig {
            listen: "127.0.0.1:0".into(),
            local_token: local_token.into(),
        },
        backends: backends
            .into_iter()
            .enumerate()
            .map(|(i, (name, url, token))| zone_router::config::Backend {
                name: name.into(),
                url: url.into(),
                token: token.into(),
                active: i == 0,
            })
            .collect(),
    };
    Arc::new(RwLock::new(
        zone_router::state::AppState::new(config, PathBuf::from("/tmp/test-config.toml")).unwrap(),
    ))
}
