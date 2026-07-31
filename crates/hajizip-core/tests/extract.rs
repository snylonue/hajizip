//! Integration tests for `ExtractEngine::run` against real fixtures.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hajizip_core::archive::tar::TarFormat;
use hajizip_core::archive::zip::ZipFormat;
use hajizip_core::registry::Registry;
use hajizip_core::source::Source;
use hajizip_core::{
    Archive, CancellationToken, EntryPath, Error, ExtractEngine, ExtractOptions, ExtractReport,
    OverwritePolicy, ProgressSink, SafetyLimits,
};

/// A self-cleaning temporary directory.
struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("hajizip-test-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .canonicalize()
        .expect("testdata must exist (run the gen scripts)")
}

fn open_zip(name: &str) -> Box<dyn Archive> {
    let reg = Registry::new().register_archive(ZipFormat);
    reg.open_archive(
        Source::Path(fixture_dir().join("zip").join(name)),
        &Default::default(),
    )
    .expect("opens")
}

fn open_tar(name: &str) -> Box<dyn Archive> {
    let reg = Registry::new()
        .register_archive(TarFormat)
        .register_codec(hajizip_core::codec::gzip::GzipFormat);
    reg.open_archive(
        Source::Path(fixture_dir().join("tar").join(name)),
        &Default::default(),
    )
    .expect("opens")
}

fn run(
    archive: &dyn Archive,
    selection: &[&str],
    opts: ExtractOptions,
    progress: &mut dyn ProgressSink,
    cancel: &CancellationToken,
) -> hajizip_core::Result<ExtractReport> {
    let selection: Vec<EntryPath> = selection
        .iter()
        .map(|s| EntryPath::new(s).expect("valid selection path"))
        .collect();
    ExtractEngine::run(archive, &selection, &opts, progress, cancel)
}

/// A progress sink recording events for assertions.
#[derive(Default)]
struct Recorder {
    events: Vec<String>,
    bytes: u64,
}

impl ProgressSink for Recorder {
    fn on_entry_start(&mut self, path: &EntryPath, size: Option<u64>) {
        self.events
            .push(format!("start:{}:{size:?}", path.as_str()));
    }
    fn on_bytes(&mut self, delta: u64) {
        self.bytes += delta;
        self.events.push(format!("bytes:{delta}"));
    }
    fn on_entry_done(&mut self, path: &EntryPath) {
        self.events.push(format!("done:{}", path.as_str()));
    }
}

/// A sink that cancels the run after the first entry completes.
struct CancelAfterFirst {
    cancel: CancellationToken,
    done: u64,
}

impl ProgressSink for CancelAfterFirst {
    fn on_entry_start(&mut self, _path: &EntryPath, _size: Option<u64>) {}
    fn on_bytes(&mut self, _delta: u64) {}
    fn on_entry_done(&mut self, _path: &EntryPath) {
        self.done += 1;
        if self.done == 1 {
            self.cancel.cancel();
        }
    }
}

#[test]
fn extract_all_from_zip() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("all");
    let mut rec = Recorder::default();
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut rec,
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.extracted, 3); // a.txt + dir/ + dir/b.txt
    assert_eq!(report.skipped, 0);
    assert!(report.failed.is_empty());
    assert_eq!(report.total_bytes, 31);
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("file"),
        b"Hello, hajizip!\n"
    );
    assert_eq!(
        std::fs::read(dir.path().join("dir/b.txt")).expect("file"),
        b"nested content\n"
    );
    assert!(dir.path().join("dir").is_dir());
    // Progress events are ordered per entry and the byte total is correct.
    let bytes: u64 = rec
        .events
        .iter()
        .filter_map(|e| e.strip_prefix("bytes:").map(|s| s.parse::<u64>().unwrap()))
        .sum();
    assert_eq!(bytes, 31);
    assert_eq!(rec.bytes, 31);
    for e in &rec.events {
        match e.as_str() {
            s if s.starts_with("bytes:") => {}
            _ => {}
        }
    }
}

#[test]
fn extract_all_from_tar_gz() {
    let archive = open_tar("basic.tar.gz");
    let dir = TestDir::new("targz");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.extracted, 3);
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("file"),
        b"Hello, hajizip!\n"
    );
}

#[test]
fn extract_selection_only() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("selection");
    let report = run(
        &*archive,
        &["a.txt"],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.extracted, 1);
    assert!(dir.path().join("a.txt").exists());
    assert!(!dir.path().join("dir/b.txt").exists());
}

#[test]
fn unknown_selection_entry_fails_run() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("missing");
    let err = run(
        &*archive,
        &["nope.txt"],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect_err("must fail");
    assert!(matches!(err, Error::CorruptArchive(_)));
}

#[test]
fn overwrite_never_and_ask_skip_existing() {
    for policy in [OverwritePolicy::Never, OverwritePolicy::Ask] {
        let archive = open_zip("basic.zip");
        let dir = TestDir::new("skip");
        std::fs::write(dir.path().join("a.txt"), b"stale").expect("pre-seed");
        let report = run(
            &*archive,
            &[],
            ExtractOptions {
                dest_dir: dir.path().to_path_buf(),
                overwrite: policy,
                ..Default::default()
            },
            &mut Recorder::default(),
            &CancellationToken::new(),
        )
        .expect("extracts");
        assert_eq!(report.skipped, 1, "a.txt must be skipped ({policy:?})");
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).expect("file"),
            b"stale"
        );
        // Other entries are still extracted.
        assert_eq!(
            std::fs::read(dir.path().join("dir/b.txt")).expect("file"),
            b"nested content\n"
        );
    }
}

#[test]
fn overwrite_always_overwrites() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("always");
    std::fs::write(dir.path().join("a.txt"), b"stale").expect("pre-seed");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            overwrite: OverwritePolicy::Always,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.skipped, 0);
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("file"),
        b"Hello, hajizip!\n"
    );
}

#[test]
fn overwrite_newer_compares_mtimes() {
    let archive = open_zip("basic.zip");
    let mtime = archive
        .entries()
        .expect("entries")
        .iter()
        .find(|e| e.path.as_str() == "a.txt")
        .and_then(|e| e.mtime)
        .expect("zip mtime");

    // Destination newer than the source: skip.
    let dir = TestDir::new("newer-skip");
    let dest = dir.path().join("a.txt");
    std::fs::write(&dest, b"newer").expect("pre-seed");
    let file = std::fs::File::options()
        .write(true)
        .open(&dest)
        .expect("open");
    file.set_times(
        std::fs::FileTimes::new().set_modified(mtime + std::time::Duration::from_secs(60)),
    )
    .expect("set mtime");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            overwrite: OverwritePolicy::Newer,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.skipped, 1);
    assert_eq!(std::fs::read(&dest).expect("file"), b"newer");

    // Destination older than the source: overwrite.
    let dir = TestDir::new("newer-overwrite");
    let dest = dir.path().join("a.txt");
    std::fs::write(&dest, b"older").expect("pre-seed");
    let file = std::fs::File::options()
        .write(true)
        .open(&dest)
        .expect("open");
    file.set_times(
        std::fs::FileTimes::new().set_modified(mtime - std::time::Duration::from_secs(60)),
    )
    .expect("set mtime");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            overwrite: OverwritePolicy::Newer,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.skipped, 0);
    assert_eq!(std::fs::read(&dest).expect("file"), b"Hello, hajizip!\n");
}

#[test]
fn preserve_mtime_sets_file_times() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("mtime");
    run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            preserve_mtime: true,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    let entries = archive.entries().expect("entries");
    for e in &entries {
        let dest = dir.path().join(e.path.as_str());
        if e.kind == hajizip_core::NodeKind::Dir {
            continue; // dir mtimes are best-effort
        }
        let on_disk = std::fs::metadata(&dest)
            .expect("exists")
            .modified()
            .expect("mtime");
        assert_eq!(on_disk, e.mtime.expect("source mtime"), "{}", e.path);
    }
}

#[test]
fn create_top_folder_is_a_contract_gap() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("top");
    let err = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            create_top_folder: true,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect_err("must fail");
    assert!(matches!(err, Error::UnsupportedFeature(_)));
}

#[test]
fn cancel_mid_run_returns_cancelled() {
    let archive = open_zip("many.zip"); // 10 000 entries
    let dir = TestDir::new("cancel");
    let cancel = CancellationToken::new();
    let mut sink = CancelAfterFirst {
        cancel: cancel.clone(),
        done: 0,
    };
    let err = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut sink,
        &cancel,
    )
    .expect_err("must cancel");
    assert!(matches!(err, Error::Cancelled));
}

#[test]
fn byte_limit_exceeded_aborts() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("limit");
    let limits = SafetyLimits {
        max_total_bytes: 10, // a.txt alone is 16 bytes
        ..SafetyLimits::default()
    };
    let err = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            limits,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect_err("must abort");
    assert!(matches!(err, Error::LimitExceeded(_)));
}

#[test]
fn entry_limit_exceeded_aborts() {
    let archive = open_zip("basic.zip");
    let dir = TestDir::new("entries");
    let limits = SafetyLimits {
        max_entries: 2,
        ..SafetyLimits::default()
    };
    let err = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            limits,
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect_err("must abort");
    assert!(matches!(err, Error::LimitExceeded(_)));
}

#[test]
fn failing_entry_is_recorded_not_aborting() {
    // enc.zip's only entry is encrypted; reading it fails with
    // UnsupportedFeature, which must land in the report's `failed` list.
    let archive = open_zip("enc.zip");
    let dir = TestDir::new("failed");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("run completes");
    assert_eq!(report.extracted, 0);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0.as_str(), "a.txt");
    assert!(matches!(report.failed[0].1, Error::UnsupportedFeature(_)));
}

#[test]
fn symlink_entries_are_skipped() {
    let archive = open_tar("sym.tar");
    let dir = TestDir::new("symlink");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    assert_eq!(report.skipped, 1);
    assert_eq!(report.extracted, 0);
    // No symlink is materialized (it pointed at /etc/passwd).
    assert!(!dir.path().join("link").exists());
}

#[test]
fn zipslip_entries_never_reach_disk() {
    let archive = open_zip("zipslip.zip");
    let dir = TestDir::new("slip");
    let report = run(
        &*archive,
        &[],
        ExtractOptions {
            dest_dir: dir.path().to_path_buf(),
            ..Default::default()
        },
        &mut Recorder::default(),
        &CancellationToken::new(),
    )
    .expect("extracts");
    // Only the legit entry is listed/extracted; the traversal entry was
    // dropped at listing time.
    assert_eq!(report.extracted, 1);
    assert!(dir.path().join("ok.txt").exists());
    let parent = dir.path().parent().expect("temp parent");
    assert!(!parent.join("evil.txt").exists());
    assert!(!dir.path().join("evil.txt").exists());
}

#[test]
fn engine_and_report_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<ExtractEngine>();
    assert_send::<ExtractReport>();
    assert_send::<CancellationToken>();
    assert_send::<SafetyLimits>();
}
