use crate::*;

impl BackendScope {
    /// Inputs are non-secret identity strings, never connection configurations.
    pub fn new(
        protocol: String,
        endpoint_identity: String,
        root_identity: String,
        generation: u64,
    ) -> Result<Self, FileError> {
        // Reject obvious URL credentials/query fragments. Fingerprints are allowed.
        if protocol.is_empty()
            || endpoint_identity.is_empty()
            || root_identity.is_empty()
            || endpoint_identity.contains(['@', '?', '#'])
        {
            return Err(FileError::new(
                FileErrorKind::InvalidInput,
                Operation::Scope,
                MutationOutcome::NotApplied,
            ));
        }
        Ok(Self {
            protocol,
            endpoint_identity,
            root_identity,
            generation,
        })
    }
    pub fn id(&self) -> ScopeId {
        ScopeId(self.clone())
    }
    pub fn protocol(&self) -> &str {
        &self.protocol
    }
    pub fn endpoint_identity(&self) -> &str {
        &self.endpoint_identity
    }
    pub fn root_identity(&self) -> &str {
        &self.root_identity
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn entry(&self, opaque: impl Into<String>) -> EntryId {
        EntryId {
            scope: self.id(),
            opaque: opaque.into(),
        }
    }
    pub fn validate_entry(&self, entry: &EntryId, operation: Operation) -> Result<(), FileError> {
        if entry.scope == self.id() {
            Ok(())
        } else {
            Err(FileError::new(
                FileErrorKind::ScopeMismatch,
                operation,
                MutationOutcome::NotApplied,
            ))
        }
    }
}
impl EntryId {
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }
    pub fn opaque(&self) -> &str {
        &self.opaque
    }
}
impl Revision {
    pub fn new(opaque: impl Into<String>) -> Self {
        Self(opaque.into())
    }
    pub fn opaque(&self) -> &str {
        &self.0
    }
}
impl ReadRange {
    pub fn new(start: u64, end: u64) -> Result<Self, FileError> {
        if start >= end {
            return Err(FileError::new(
                FileErrorKind::InvalidInput,
                Operation::Read,
                MutationOutcome::NotApplied,
            ));
        }
        Ok(Self { start, end })
    }
    pub fn start(self) -> u64 {
        self.start
    }
    pub fn end(self) -> u64 {
        self.end
    }
    pub fn len(self) -> u64 {
        self.end - self.start
    }
    pub fn is_empty(self) -> bool {
        false
    }
}
impl Condition {
    pub fn kind(&self) -> ConditionKind {
        match self {
            Self::Any => ConditionKind::Any,
            Self::CreateOnly => ConditionKind::CreateOnly,
            Self::Match(_) => ConditionKind::Match,
        }
    }
}
