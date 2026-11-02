use crate::{FileErrorKind, MutationOutcome, Operation, RetryClass};
use std::{error::Error, fmt};

pub type ErrorSource = Box<dyn Error + Send + Sync + 'static>;

/// Evidence from a failed cleanup, independent of the primary error chain.
#[derive(Debug)]
pub struct CleanupFailure {
    pub operation: Operation,
    pub status: Option<u16>,
    pub provider_code: Option<String>,
    /// Non-secret recovery handle, if the provider exposes one.
    pub recovery_id: Option<String>,
    pub source: ErrorSource,
}

#[derive(Debug)]
pub struct FileError {
    pub kind: FileErrorKind,
    pub operation: Operation,
    pub status: Option<u16>,
    pub provider_code: Option<String>,
    pub outcome: MutationOutcome,
    pub retry: RetryClass,
    pub cleanup: Vec<CleanupFailure>,
    source: Option<ErrorSource>,
}

impl FileError {
    pub fn new(kind: FileErrorKind, operation: Operation, outcome: MutationOutcome) -> Self {
        Self {
            kind,
            operation,
            outcome,
            status: None,
            provider_code: None,
            retry: RetryClass::Never,
            cleanup: Vec::new(),
            source: None,
        }
    }

    pub fn into_source(self) -> Option<ErrorSource> {
        self.source
    }

    pub fn unsupported(operation: Operation) -> Self {
        Self::new(
            FileErrorKind::Unsupported,
            operation,
            MutationOutcome::NotApplied,
        )
    }

    pub fn with_source(mut self, source: ErrorSource) -> Self {
        self.source = Some(source);
        self
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Source and provider diagnostics are for structured logging, not nested UI text.
        write!(f, "{:?}: {:?}", self.operation, self.kind)
    }
}
impl Error for FileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
