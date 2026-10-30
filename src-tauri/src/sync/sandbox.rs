//! Linux Landlock confinement for synchronization workers.
//!
//! The policy is deliberately installed in a dedicated OS thread: Landlock's
//! `restrict_self` affects that thread only and does not change the process or
//! the Tokio runtime threads.

use std::fs::File;

use crate::sync::sandbox_types::SandboxError;

#[cfg(target_os = "linux")]
use std::os::fd::AsFd;

#[cfg(target_os = "linux")]
use landlock::{
    ABI, Access, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus,
};

/// Runs `operation` in a thread confined to `root` and `private`.
#[cfg(target_os = "linux")]
pub fn run_confined<T, F>(root: File, private: File, operation: F) -> Result<T, SandboxError>
where
    T: Send + 'static,
    F: FnOnce(File, File) -> Result<T, SandboxError> + Send + 'static,
{
    let worker = std::thread::spawn(move || {
        let rights = Access::from_all(ABI::V3);
        let ruleset = Ruleset::default()
            .handle_access(rights)
            .map_err(SandboxError::from)?
            .set_compatibility(CompatLevel::HardRequirement)
            .create()
            .map_err(SandboxError::from)?;
        let ruleset = ruleset
            .add_rule(PathBeneath::new(root.as_fd(), rights))
            .map_err(SandboxError::from)?
            .add_rule(PathBeneath::new(private.as_fd(), rights))
            .map_err(SandboxError::from)?;
        let status = ruleset.restrict_self().map_err(SandboxError::from)?;
        if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
            return Err(SandboxError::Unsupported);
        }
        operation(root, private)
    });

    match worker.join() {
        Ok(result) => result,
        Err(_) => Err(SandboxError::JoinPanic),
    }
}

/// Landlock is Linux-only; callers get an explicit capability error elsewhere.
#[cfg(not(target_os = "linux"))]
pub fn run_confined<T, F>(_root: File, _private: File, _operation: F) -> Result<T, SandboxError>
where
    T: Send + 'static,
    F: FnOnce(File, File) -> Result<T, SandboxError> + Send + 'static,
{
    Err(SandboxError::Unsupported)
}
