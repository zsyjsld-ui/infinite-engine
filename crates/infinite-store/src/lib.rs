//! Standalone persistence primitives. This crate never opens a game's existing
//! saves implicitly and does not publish changes into a live simulation.
mod codec;
mod journal;
mod service;
pub use codec::{Change, DecodedFrame, FrameKind, JournalMark, Limits, RecordKey,
    StoreError, decode_frame, encode_checkpoint, encode_transaction};
pub use journal::{CheckpointJob,PreparedCheckpoint,CommitReceipt,Durability,Journal,JournalError,JournalOptions,Records,Recovered,recover};
pub use service::{JournalService,ServiceOptions,ServiceError,SubmitError,RejectedCommit,Ticket,Completion,CompletionKind,Shutdown};
#[cfg(feature="fault-injection")]
pub use journal::{FaultHook,IoPoint,IoStage};
