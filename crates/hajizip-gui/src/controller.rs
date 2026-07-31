//! GUI controller: the UI-independent state machine that drives `hajizip-core`.
//!
//! The controller is deliberately split into two halves:
//!
//! * [`ControllerCore`] — a pure, synchronous state machine that turns an
//!   [`Intent`] into a list of [`Event`]s. It owns the composed [`Registry`]
//!   (the composition root) and the current archive handle. It contains no
//!   threads and no Dioxus types, so it can be unit-tested with fake
//!   `Archive` / `ArchiveFormat` implementations (see `test-plan.md` §11).
//! * [`spawn_controller`] — the transport layer used by the UI. It runs a
//!   [`ControllerCore`] on a dedicated worker thread and bridges intents and
//!   events to the UI thread over channels, so long-running core calls never
//!   block the interface (see `architecture.md` §5.5).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hajizip_core::{
    Archive, CancellationToken, EntryMeta, EntryPath, Error as CoreError, ExtractReport,
    FilenameEncoding, OpenOptions, Registry, Secret, Source,
};

use crate::config::AppConfig;

/// A user intention submitted to the controller.
//
// Only `Open` is wired into the UI in M0; the remaining variants are part of
// the frozen controller contract and are exercised from M1 onwards (see
// architecture.md §5). They are kept now so the Intent surface is stable.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Intent {
    /// Open a top-level archive from disk, optionally with a password.
    Open {
        /// Path to the archive file.
        path: PathBuf,
        /// Password for encrypted archives, if any.
        password: Option<String>,
    },
    /// Enter a child directory or nested archive.
    Enter {
        /// Path of the entry to enter.
        path: EntryPath,
    },
    /// Go back up one navigation level.
    Back,
    /// Extract a selection of entries (empty means all) to a directory.
    Extract {
        /// Entries to extract; empty means the whole archive.
        selection: Vec<EntryPath>,
        /// Destination directory.
        dest_dir: PathBuf,
    },
    /// Cancel the in-flight operation.
    Cancel,
    /// Change the filename decoding strategy and refresh the view.
    SetEncoding(FilenameEncoding),
}

/// A progress snapshot emitted during a long-running operation.
//
// Populated once extraction reports progress (M1+); part of the stable Event
// contract today.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ProgressUpdate {
    /// Path of the entry currently being processed, if any.
    pub current: Option<EntryPath>,
    /// Bytes processed so far.
    pub bytes_done: u64,
    /// Total bytes expected, if known.
    pub bytes_total: Option<u64>,
    /// Entries completed so far.
    pub entries_done: u64,
}

/// An event produced by the controller for the UI to render.
//
// `Progress` / `Done` are emitted once extraction lands (M1+); kept now so the
// Event surface is stable.
#[allow(dead_code)]
#[derive(Debug)]
pub enum Event {
    /// An archive was opened successfully.
    Opened {
        /// Display name of the archive (e.g. its file name).
        name: String,
        /// Flat listing of the archive's entries (the UI builds the tree).
        entries: Vec<EntryMeta>,
    },
    /// The archive is encrypted and needs a password to open.
    PasswordRequired {
        /// Path of the archive that needs a password.
        path: PathBuf,
    },
    /// The supplied password was rejected.
    WrongPassword {
        /// Path of the archive whose password was wrong.
        path: PathBuf,
    },
    /// Progress of an in-flight extraction.
    Progress(ProgressUpdate),
    /// An extraction run completed.
    Done(ExtractReport),
    /// The in-flight operation was cancelled.
    Cancelled,
    /// The requested capability is not available yet (e.g. no format matched,
    /// or a milestone feature is not implemented).
    Unsupported(String),
    /// A generic, user-presentable error.
    Error(String),
}

/// The synchronous, UI-independent controller state machine.
pub struct ControllerCore {
    registry: Arc<Registry>,
    config: AppConfig,
    /// The currently open archive, if any.
    current: Option<Arc<dyn Archive>>,
    /// Token used to cancel in-flight operations.
    cancel: CancellationToken,
}

impl ControllerCore {
    /// Create a controller composing the given format registry and config.
    pub fn new(registry: Arc<Registry>, config: AppConfig) -> Self {
        Self {
            registry,
            config,
            current: None,
            cancel: CancellationToken::new(),
        }
    }

    /// The active configuration.
    #[allow(dead_code)] // Used by the settings UI from M1+; exercised in tests today.
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Process a single intent, returning the events it produced.
    ///
    /// This is the heart of the controller and is safe to call from tests
    /// without any UI or threads.
    pub fn handle(&mut self, intent: &Intent) -> Vec<Event> {
        match intent {
            Intent::Open { path, password } => self.open(path, password.as_deref()),
            Intent::SetEncoding(encoding) => {
                self.config.filename_encoding = *encoding;
                // Re-decoding entry names is a per-format concern handled when
                // the archive is (re)opened; nothing to emit for now.
                Vec::new()
            }
            // Navigation and extraction land in later milestones (see
            // architecture.md §5). Until then, report them as unsupported so
            // the UI can present a friendly message instead of crashing.
            Intent::Enter { .. } => vec![Event::Unsupported(
                "archive navigation is not implemented yet".into(),
            )],
            Intent::Back => vec![Event::Unsupported(
                "archive navigation is not implemented yet".into(),
            )],
            Intent::Extract { .. } => vec![Event::Unsupported(
                "extraction is not implemented yet".into(),
            )],
            Intent::Cancel => {
                self.cancel.cancel();
                vec![Event::Cancelled]
            }
        }
    }

    /// Open a top-level archive and list its entries.
    fn open(&mut self, path: &Path, password: Option<&str>) -> Vec<Event> {
        let opts = OpenOptions {
            password: password.map(Secret::new),
            encoding: self.config.filename_encoding,
        };

        let archive = match self
            .registry
            .open_archive(Source::Path(path.to_path_buf()), &opts)
        {
            Ok(archive) => archive,
            Err(CoreError::PasswordRequired) => {
                return vec![Event::PasswordRequired {
                    path: path.to_path_buf(),
                }];
            }
            Err(CoreError::WrongPassword) => {
                return vec![Event::WrongPassword {
                    path: path.to_path_buf(),
                }];
            }
            Err(e) => return vec![Event::Error(e.to_string())],
        };

        let archive: Arc<dyn Archive> = Arc::from(archive);
        match archive.entries() {
            Ok(entries) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.current = Some(archive);
                vec![Event::Opened { name, entries }]
            }
            Err(e) => vec![Event::Error(e.to_string())],
        }
    }
}

/// A handle used by the UI to submit intents to the background controller.
#[derive(Clone)]
pub struct ControllerHandle {
    /// Send an intent to the worker thread. Never blocks.
    pub commands: tokio::sync::mpsc::UnboundedSender<Intent>,
}

/// Spawn a [`ControllerCore`] on a dedicated worker thread.
///
/// Intents are fed in over the returned [`ControllerHandle`]; events come out
/// of the returned receiver, which the UI drains on the reactive thread. The
/// worker owns the core and all blocking `hajizip-core` calls happen there,
/// keeping the interface responsive.
pub fn spawn_controller(
    registry: Arc<Registry>,
    config: AppConfig,
) -> (
    ControllerHandle,
    tokio::sync::mpsc::UnboundedReceiver<Event>,
) {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Intent>();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    std::thread::spawn(move || {
        let mut core = ControllerCore::new(registry, config);
        // `blocking_recv` is fine here: this is a dedicated OS thread whose
        // whole job is to wait for and process intents.
        while let Some(intent) = cmd_rx.blocking_recv() {
            for event in core.handle(&intent) {
                if event_tx.send(event).is_err() {
                    // The UI dropped the receiver; stop the worker.
                    return;
                }
            }
        }
    });

    (ControllerHandle { commands: cmd_tx }, event_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hajizip_core::{
        ArchiveFormat, Capabilities, Error, NodeKind, NodeRef, OpenOptions, Result,
    };
    use std::io::{Read, Write};

    /// Build a minimal file entry for tests.
    fn meta(path: &str) -> EntryMeta {
        EntryMeta {
            path: EntryPath::new(path).unwrap(),
            raw_name: path.as_bytes().to_vec(),
            kind: NodeKind::File,
            uncompressed_size: Some(0),
            compressed_size: None,
            mtime: None,
            mode: None,
            crc: None,
            encrypted: false,
            comment: None,
        }
    }

    fn caps() -> Capabilities {
        Capabilities {
            random_access: true,
            encrypted: false,
            needs_password: false,
            can_write: false,
        }
    }

    /// A fake archive that simply returns a fixed entry list.
    struct FakeArchive {
        entries: Vec<EntryMeta>,
    }

    impl Archive for FakeArchive {
        fn entries(&self) -> Result<Vec<EntryMeta>> {
            Ok(self.entries.clone())
        }
        fn root(&self) -> Result<NodeRef> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn node(&self, _path: &EntryPath) -> Result<NodeRef> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn reader<'s>(&'s self, _entry: &EntryMeta) -> Result<Box<dyn Read + Send + 's>> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn extract_to(&self, _entry: &EntryMeta, _sink: &mut dyn Write) -> Result<u64> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn open_nested(&self, _entry: &EntryMeta, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn capabilities(&self) -> Capabilities {
            caps()
        }
    }

    /// A format that always matches and opens a [`FakeArchive`].
    struct OkFormat {
        entries: Vec<EntryMeta>,
    }

    impl ArchiveFormat for OkFormat {
        fn id(&self) -> &str {
            "fake"
        }
        fn display_name(&self) -> &str {
            "Fake"
        }
        fn extensions(&self) -> &[&str] {
            &["fake"]
        }
        fn matches(&self, _head: &[u8], _ext: Option<&str>) -> bool {
            true
        }
        fn open(&self, _src: Source, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Ok(Box::new(FakeArchive {
                entries: self.entries.clone(),
            }))
        }
    }

    /// A format that always matches but demands a password.
    struct PasswordFormat;

    impl ArchiveFormat for PasswordFormat {
        fn id(&self) -> &str {
            "locked"
        }
        fn display_name(&self) -> &str {
            "Locked"
        }
        fn extensions(&self) -> &[&str] {
            &["locked"]
        }
        fn matches(&self, _head: &[u8], _ext: Option<&str>) -> bool {
            true
        }
        fn open(&self, _src: Source, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Err(Error::PasswordRequired)
        }
    }

    /// Write a small temp file so `Source::Path` can sniff real bytes.
    fn temp_archive(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hajizip-gui-test-{name}"));
        std::fs::write(&path, b"some archive bytes").unwrap();
        path
    }

    #[test]
    fn open_with_registered_format_emits_opened() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt"), meta("dir/b.txt")],
        });
        let mut core = ControllerCore::new(Arc::new(registry), AppConfig::default());

        let path = temp_archive("ok.fake");
        let events = core.handle(&Intent::Open {
            path: path.clone(),
            password: None,
        });

        match &events[..] {
            [Event::Opened { name, entries }] => {
                assert_eq!(name, &path.file_name().unwrap().to_string_lossy());
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn open_with_empty_registry_emits_error() {
        let mut core = ControllerCore::new(Arc::new(Registry::new()), AppConfig::default());
        let events = core.handle(&Intent::Open {
            path: PathBuf::from("/nonexistent/definitely-missing.zip"),
            password: None,
        });
        assert!(
            matches!(&events[..], [Event::Error(_)]),
            "expected Error, got {events:?}"
        );
    }

    #[test]
    fn open_encrypted_without_password_emits_password_required() {
        let registry = Registry::new().register_archive(PasswordFormat);
        let mut core = ControllerCore::new(Arc::new(registry), AppConfig::default());

        let path = temp_archive("locked.fake");
        let events = core.handle(&Intent::Open {
            path: path.clone(),
            password: None,
        });

        assert!(
            matches!(&events[..], [Event::PasswordRequired { .. }]),
            "expected PasswordRequired, got {events:?}"
        );
    }

    #[test]
    fn unimplemented_intents_are_reported_not_panicking() {
        let mut core = ControllerCore::new(Arc::new(Registry::new()), AppConfig::default());
        let events = core.handle(&Intent::Extract {
            selection: vec![],
            dest_dir: PathBuf::from("/tmp"),
        });
        assert!(matches!(&events[..], [Event::Unsupported(_)]));
    }

    #[test]
    fn set_encoding_updates_config() {
        let mut core = ControllerCore::new(Arc::new(Registry::new()), AppConfig::default());
        assert_eq!(core.config().filename_encoding, FilenameEncoding::Auto);
        core.handle(&Intent::SetEncoding(FilenameEncoding::Forced(
            hajizip_core::Codepage::Gbk,
        )));
        assert_eq!(
            core.config().filename_encoding,
            FilenameEncoding::Forced(hajizip_core::Codepage::Gbk)
        );
    }
}
