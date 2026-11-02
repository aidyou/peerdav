use crate::*;

/// Adapters must run the matching capability/scope check and context check
/// before provider I/O or polling upload bodies. Unsupported defaults perform
/// neither. Successful overrides must enforce budgets, bind returned identities
/// and cursors, preserve stream errors, and report actual mutation outcomes.
/// An authorized, replayable handle. Implementations must not reopen ambient paths.
/// Reads are bounded by `length`; validation checks the original source identity.
pub trait ReplayableSource: Send + Sync {
    fn size(&self) -> u64;
    fn resume_key(&self) -> &str;
    fn read_at(&self, offset: u64, length: usize)
    -> BoxFuture<'_, Result<bytes::Bytes, FileError>>;
    fn validate(&self) -> BoxFuture<'_, Result<(), FileError>>;
}

/// Unified extension for resumable writes requiring a replayable authorized source.
/// Streaming support refers to bounded memory, not conditional atomic replacement.
pub trait ReplayableWriteBackend: FileBackend {
    fn write_source<'a>(
        &'a self,
        context: OperationContext,
        request: SourceWriteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>>;
}

pub trait FileBackend: Send + Sync {
    fn scope(&self) -> &BackendScope;
    fn capabilities(&self) -> &Capabilities;
    fn list<'a>(
        &'a self,
        _context: OperationContext,
        _request: ListRequest,
    ) -> BoxFuture<'a, Result<Page, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::List)) })
    }
    fn stat<'a>(
        &'a self,
        _context: OperationContext,
        _entry: EntryId,
    ) -> BoxFuture<'a, Result<Entry, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::Stat)) })
    }
    fn read<'a>(
        &'a self,
        _context: OperationContext,
        _request: ReadRequest,
    ) -> BoxFuture<'a, Result<ReadResponse, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::Read)) })
    }
    fn write<'a>(
        &'a self,
        _context: OperationContext,
        _request: WriteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::Write)) })
    }
    fn create_dir<'a>(
        &'a self,
        _context: OperationContext,
        _request: CreateDirRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::CreateDir)) })
    }
    fn delete<'a>(
        &'a self,
        _context: OperationContext,
        _request: DeleteRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::Delete)) })
    }
    fn move_entry<'a>(
        &'a self,
        _context: OperationContext,
        _request: MoveRequest,
    ) -> BoxFuture<'a, Result<MutationResult, FileError>> {
        Box::pin(async { Err(FileError::unsupported(Operation::Move)) })
    }
}
