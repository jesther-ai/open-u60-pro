//! Build identity embedded in every agent and release artifact.

use serde::Serialize;

/// A release build sets these three values in CI. Local builds keep an
/// explicit development identity instead of pretending to be a release.
pub const VERSION: &str = env!("ZTE_AGENT_BUILD_VERSION");
pub const COMMIT: &str = env!("ZTE_AGENT_BUILD_COMMIT");
pub const TARGET: &str = env!("ZTE_AGENT_BUILD_TARGET");

#[derive(Clone, Debug, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    pub commit: &'static str,
    pub target: &'static str,
}

pub const fn current() -> BuildInfo {
    BuildInfo {
        version: VERSION,
        commit: COMMIT,
        target: TARGET,
    }
}

pub fn display() -> String {
    format!("{} ({}, {})", VERSION, COMMIT, TARGET)
}
