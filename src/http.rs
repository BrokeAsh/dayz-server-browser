//! The HTTP client shared by the list fetchers and the Steam Web API calls.

use std::sync::OnceLock;
use std::time::Duration;

pub const USER_AGENT: &str = concat!("dayz-browser/", env!("CARGO_PKG_VERSION"));

pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(USER_AGENT)
            .build()
            .new_agent()
    })
}
