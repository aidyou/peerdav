use crate::*;

impl Capabilities {
    fn require(supported: bool, operation: Operation) -> Result<(), FileError> {
        if supported {
            Ok(())
        } else {
            Err(FileError::unsupported(operation))
        }
    }
    pub fn check_list(&self, scope: &BackendScope, request: &ListRequest) -> Result<(), FileError> {
        scope.validate_entry(&request.parent, Operation::List)?;
        request.validate_cursor()?;
        Self::require(
            self.list
                && (request.query.is_none() || self.list_query)
                && (request.prefix.is_none() || self.list_prefix)
                && (request.cursor.is_none() || self.list_cursor),
            Operation::List,
        )
    }
    pub fn check_stat(&self, scope: &BackendScope, entry: &EntryId) -> Result<(), FileError> {
        scope.validate_entry(entry, Operation::Stat)?;
        Self::require(self.stat, Operation::Stat)
    }
    pub fn check_read(&self, scope: &BackendScope, request: &ReadRequest) -> Result<(), FileError> {
        scope.validate_entry(&request.entry, Operation::Read)?;
        Self::require(
            self.read && (request.range.is_none() || self.range_read),
            Operation::Read,
        )
    }
    pub fn check_write(
        &self,
        scope: &BackendScope,
        request: &WriteRequest,
    ) -> Result<(), FileError> {
        scope.validate_entry(&request.entry, Operation::Write)?;
        Self::require(
            self.writes
                .iter()
                .any(|support| match (*support, request.mode) {
                    (
                        WriteSupport::BoundedAtomic {
                            condition,
                            max_bytes,
                        },
                        WriteMode::BoundedAtomic {
                            max_bytes: requested,
                        },
                    ) => condition == request.condition.kind() && requested <= max_bytes,
                    (WriteSupport::Streaming { condition }, WriteMode::Streaming) => {
                        condition == request.condition.kind()
                    }
                    _ => false,
                }),
            Operation::Write,
        )
    }
    pub fn check_create_dir(
        &self,
        scope: &BackendScope,
        request: &CreateDirRequest,
    ) -> Result<(), FileError> {
        scope.validate_entry(&request.entry, Operation::CreateDir)?;
        Self::require(
            self.create_dir.contains(&request.condition.kind()),
            Operation::CreateDir,
        )
    }
    pub fn check_delete(
        &self,
        scope: &BackendScope,
        request: &DeleteRequest,
    ) -> Result<(), FileError> {
        scope.validate_entry(&request.entry, Operation::Delete)?;
        if request.entry.opaque().trim_matches('/').is_empty() {
            return Err(FileError::new(
                FileErrorKind::InvalidInput,
                Operation::Delete,
                MutationOutcome::NotApplied,
            ));
        }
        Self::require(
            self.deletes.contains(&DeleteSupport {
                mode: request.mode,
                condition: request.condition.kind(),
            }),
            Operation::Delete,
        )
    }
    pub fn check_move(&self, scope: &BackendScope, request: &MoveRequest) -> Result<(), FileError> {
        scope.validate_entry(&request.source, Operation::Move)?;
        scope.validate_entry(&request.destination, Operation::Move)?;
        Self::require(
            self.moves.contains(&MoveSupport {
                atomicity: request.atomicity,
                source_condition: request.source_condition.kind(),
                destination_condition: request.destination_condition.kind(),
            }),
            Operation::Move,
        )
    }
}
