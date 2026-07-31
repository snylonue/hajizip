//! `hajizip-core` is the UI-agnostic core of the hajizip archive tool.
//!
//! It defines unified interfaces for reading and extracting a variety of
//! archive and compression formats, plus recursive access to files, folders
//! and nested archives. The crate is pure safe Rust and contains no `unsafe`.
//!
//! This is the M0 skeleton: interfaces and data models are defined, while
//! concrete format implementations are added in later milestones.

#![forbid(unsafe_code)]

pub mod archive;
pub mod codec;
pub mod encoding;
pub mod error;
pub mod extract;
pub mod model;
pub mod navigate;
pub mod registry;
pub mod source;

pub use archive::{Archive, Capabilities, Node, NodeRef, OpenOptions};
pub use codec::Codec;
pub use encoding::{Codepage, FilenameEncoding, Utf8Flag, decode_filename};
pub use error::{Error, Result};
pub use extract::{
    CancellationToken, ExtractEngine, ExtractOptions, ExtractReport, OverwritePolicy, ProgressSink,
    SafetyLimits,
};
pub use model::{
    CodecId, EntryMeta, EntryPath, FormatKind, Level, Location, NodeKind, Secret, Timestamp,
};
pub use navigate::{Frame, Navigator, Walk, WalkItem, WalkOptions, walk};
pub use registry::{FormatRegistry, Registry, open};
pub use source::{ReadSeek, Source};
