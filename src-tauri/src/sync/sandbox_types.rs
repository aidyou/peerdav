//! Typed errors for the Linux synchronization sandbox.

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[cfg(target_os = "linux")]
    #[error("failed to configure filesystem sandbox")]
    Ruleset(#[source] landlock::RulesetError),
    #[error("filesystem sandbox is unsupported on this platform")]
    Unsupported,
    #[error("confined worker panicked")]
    JoinPanic,
}

#[cfg(target_os = "linux")]
impl From<landlock::RulesetError> for SandboxError {
    fn from(error: landlock::RulesetError) -> Self {
        Self::Ruleset(error)
    }
}
