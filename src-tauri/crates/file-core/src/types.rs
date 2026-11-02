use std::{num::NonZeroUsize, pin::Pin, time::Instant};

use bytes::Bytes;
use futures_core::Stream;
use tokio_util::sync::CancellationToken;

use crate::FileError;

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
/// Pull-only body: adapters must not spawn unbounded producer queues or collect
/// an entire Streaming write. Each chunk and total transfer obey context budget.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, FileError>> + Send + 'static>>;

/// Opaque logical identity; never URI-encoded, normalized or interpreted here.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ScopeId(pub(crate) BackendScope);

/// Non-secret identity components only. Endpoint is a credential-free canonical
/// endpoint identity (or fingerprint), root includes bucket/prefix or peer/share.
/// Generation changes on reconnection/reconfiguration, even at the same root.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BackendScope {
    pub(crate) protocol: String,
    pub(crate) endpoint_identity: String,
    pub(crate) root_identity: String,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EntryId {
    pub(crate) scope: ScopeId,
    pub(crate) opaque: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Revision(pub(crate) String);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub id: EntryId,
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    /// Provider modification time as Unix seconds; absent when not supplied.
    pub modified: Option<i64>,
    /// Provider media type, without a second metadata request.
    pub content_type: Option<String>,
    /// Opaque provider revision, not a content hash.
    pub revision: Option<Revision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListRequest {
    pub parent: EntryId,
    /// Literal backend query/prefix; unsupported semantics must be rejected,
    /// never silently ignored or applied after a truncated listing.
    pub query: Option<String>,
    pub prefix: Option<String>,
    pub cursor: Option<ListCursor>,
    pub limit: NonZeroUsize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListCursor {
    pub(crate) binding: Box<ListRequest>,
    pub(crate) opaque: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Completeness {
    Complete,
    More(ListCursor),
    /// Incomplete and cannot resume; never safe as a recursive inventory.
    Truncated,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page {
    pub(crate) entries: Vec<Entry>,
    pub(crate) completeness: Completeness,
}

/// Nonempty half-open interval [start, end).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadRange {
    pub(crate) start: u64,
    pub(crate) end: u64,
}
#[derive(Clone, Debug)]
pub struct ReadRequest {
    pub entry: EntryId,
    pub range: Option<ReadRange>,
}
pub struct ReadResponse {
    pub entry: Entry,
    /// Actual interval returned, None for a full response. Adapters must reject
    /// ignored range requests rather than label a full body as a partial body.
    pub range: Option<ReadRange>,
    pub body: ByteStream,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Condition {
    Any,
    CreateOnly,
    Match(Revision),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionKind {
    Any,
    CreateOnly,
    Match,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteMode {
    /// Atomic publication with a hard bound; not emulated with multipart overwrite.
    BoundedAtomic { max_bytes: u64 },
    /// Bounded-memory transfer, NOT a promise of conditional atomic publication.
    Streaming,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritePhase {
    Hashing,
    Uploading,
    Committing,
    Completed,
}
pub type WriteProgress = std::sync::Arc<dyn Fn(WritePhase, u64, u64, u64) + Send + Sync>;
pub struct SourceWriteRequest {
    pub entry: EntryId,
    pub condition: Condition,
    pub mode: WriteMode,
    pub source: std::sync::Arc<dyn crate::ReplayableSource>,
    /// Non-secret caller-generated attempt identity, reused for reconciliation.
    pub session_id: String,
    pub progress: Option<WriteProgress>,
}

pub struct WriteRequest {
    pub entry: EntryId,
    pub condition: Condition,
    pub mode: WriteMode,
    pub body: ByteStream,
}
#[derive(Clone, Debug)]
pub struct CreateDirRequest {
    pub entry: EntryId,
    pub condition: Condition,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteMode {
    File,
    EmptyDirectory,
    Recursive,
}
#[derive(Clone, Debug)]
pub struct DeleteRequest {
    pub entry: EntryId,
    pub mode: DeleteMode,
    /// Recursive Match means a provider-enforced tree revision, not a HEAD then DELETE.
    pub condition: Condition,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveAtomicity {
    Atomic,
    /// Native move with explicitly non-atomic provider guarantees. Never copy-delete.
    NativeNonAtomic,
}
#[derive(Clone, Debug)]
pub struct MoveRequest {
    pub source: EntryId,
    pub destination: EntryId,
    pub source_condition: Condition,
    pub destination_condition: Condition,
    pub atomicity: MoveAtomicity,
}
#[derive(Clone, Debug)]
pub struct MutationResult {
    pub entry: Option<Entry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteSupport {
    BoundedAtomic {
        condition: ConditionKind,
        max_bytes: u64,
    },
    Streaming {
        condition: ConditionKind,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeleteSupport {
    pub mode: DeleteMode,
    pub condition: ConditionKind,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MoveSupport {
    pub atomicity: MoveAtomicity,
    pub source_condition: ConditionKind,
    pub destination_condition: ConditionKind,
}
/// Exact guarantees verified by an adapter, not guesses based on a protocol name.
/// No capability is enabled by default. Scope-local only; no cross-scope moves.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    pub list: bool,
    pub list_query: bool,
    pub list_prefix: bool,
    pub list_cursor: bool,
    pub stat: bool,
    pub read: bool,
    pub range_read: bool,
    pub writes: Vec<WriteSupport>,
    pub create_dir: Vec<ConditionKind>,
    pub deletes: Vec<DeleteSupport>,
    pub moves: Vec<MoveSupport>,
}

#[derive(Clone, Copy, Debug)]
pub struct OperationBudget {
    pub max_bytes: u64,
    pub max_chunk_bytes: NonZeroUsize,
    pub max_entries: NonZeroUsize,
}
#[derive(Clone, Debug)]
pub struct OperationContext {
    pub cancellation: CancellationToken,
    pub deadline: Option<Instant>,
    pub budget: OperationBudget,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Scope,
    List,
    Stat,
    Read,
    Write,
    CreateDir,
    Delete,
    Move,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationOutcome {
    NotApplied,
    Applied,
    Unknown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileErrorKind {
    Unsupported,
    InvalidInput,
    ScopeMismatch,
    NotFound,
    PermissionDenied,
    Precondition,
    Cancelled,
    Deadline,
    BudgetExceeded,
    Transport,
    Provider,
    Internal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryClass {
    Never,
    /// Retry only if the caller can prove replay safety (especially Unknown mutations).
    IfReplaySafe,
    AfterRefresh,
}
