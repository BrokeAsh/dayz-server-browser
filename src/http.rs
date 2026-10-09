//! The HTTP client shared by the list fetchers and the Steam Web API calls.

use std::sync::OnceLock;
use std::time::Duration;

pub const USER_AGENT: &str = concat!("dayz-browser/", env!("CARGO_PKG_VERSION"));

pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        // Generous for the body: the DZSA list is large, and a slow link may need minutes for it.
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(180)))
            .user_agent(USER_AGENT)
            .build()
            .new_agent()
    })
}
