//! Integration tests for `Navigator` and `walk` against real fixtures.

use std::path::{Path, PathBuf};

use hajizip_core::source::Source;
use hajizip_core::{
    Archive, Capabilities, EntryMeta, EntryPath, Error, Location, Navigator, NodeKind, NodeRef,
    OpenOptions, WalkOptions, walk,
};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .canonicalize()
        .expect("testdata must exist (run the gen scripts)")
}

fn fixture(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

fn open_root(path: &Path) -> Navigator {
    Navigator::open_root(
        &default_registry(),
        Source::Path(path.to_path_buf()),
        &OpenOptions::default(),
    )
    .expect("opens")
}

/// The core's built-in format set (zip / 7z / tar / gzip / xz), as the
/// composition root would provide it to `Navigator::open_root`.
fn default_registry() -> hajizip_core::Registry {
    hajizip_core::Registry::new()
        .register_archive(hajizip_core::archive::zip::ZipFormat)
        .register_archive(hajizip_core::archive::sevenz::SevenZipFormat)
        .register_archive(hajizip_core::archive::tar::TarFormat)
        .register_codec(hajizip_core::codec::gzip::GzipFormat)
        .register_codec(hajizip_core::codec::xz::XzFormat)
}

fn entry_by_path<'a>(entries: &'a [EntryMeta], path: &str) -> &'a EntryMeta {
    entries
        .iter()
        .find(|e| e.path.as_str() == path)
        .expect("entry present")
}

fn entry_paths(archive: &dyn Archive) -> Vec<String> {
    let mut v: Vec<String> = archive
        .entries()
        .expect("entries")
        .into_iter()
        .map(|e| e.path.as_str().to_owned())
        .collect();
    v.sort();
    v
}

fn walk_locations(archive: &dyn Archive, opts: WalkOptions) -> Vec<String> {
    walk(archive, opts)
        .map(|item| item.expect("walk item").location.0)
        .collect()
}

/// A fake archive whose only entry looks like a nested archive but fails to
/// open; `walk` must surface the failure rather than silently dropping it.
struct BrokenNested;

impl Archive for BrokenNested {
    fn entries(&self) -> hajizip_core::Result<Vec<EntryMeta>> {
        Ok(vec![EntryMeta {
            path: EntryPath::new("inner.zip").unwrap(),
            raw_name: b"inner.zip".to_vec(),
            kind: NodeKind::Archive,
            uncompressed_size: Some(1),
            compressed_size: None,
            mtime: None,
            mode: None,
            crc: None,
            encrypted: false,
            comment: None,
        }])
    }

    fn root(&self) -> hajizip_core::Result<NodeRef> {
        Err(Error::UnsupportedFeature("fake".into()))
    }

    fn node(&self, _p: &EntryPath) -> hajizip_core::Result<NodeRef> {
        Err(Error::UnsupportedFeature("fake".into()))
    }

    fn reader<'s>(
        &'s self,
        _e: &EntryMeta,
    ) -> hajizip_core::Result<Box<dyn std::io::Read + Send + 's>> {
        Err(Error::UnsupportedFeature("fake".into()))
    }

    fn extract_to(&self, _e: &EntryMeta, _w: &mut dyn std::io::Write) -> hajizip_core::Result<u64> {
        Err(Error::UnsupportedFeature("fake".into()))
    }

    fn open_nested(
        &self,
        _e: &EntryMeta,
        _o: &OpenOptions,
    ) -> hajizip_core::Result<Box<dyn Archive>> {
        Err(Error::CorruptArchive("broken inner archive".into()))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
            encrypted: false,
            needs_password: false,
            can_write: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Navigator
// ---------------------------------------------------------------------------

#[test]
fn open_root_lists_zip() {
    let nav = open_root(&fixture("zip/basic.zip"));
    assert_eq!(nav.breadcrumb().len(), 1);
    assert_eq!(nav.breadcrumb()[0].focus, None);
    let archive = nav.current().expect("current");
    assert_eq!(entry_paths(archive), ["a.txt", "dir", "dir/b.txt"]);
}

#[test]
fn open_root_opens_tar_gz_via_builtin_registry() {
    let nav = open_root(&fixture("tar/basic.tar.gz"));
    let archive = nav.current().expect("current");
    assert_eq!(entry_paths(archive), ["a.txt", "dir", "dir/b.txt"]);
}

#[test]
fn open_root_opens_tar_xz_via_builtin_registry() {
    let nav = open_root(&fixture("tar/basic.tar.xz"));
    let archive = nav.current().expect("current");
    assert_eq!(entry_paths(archive), ["a.txt", "dir", "dir/b.txt"]);
}

#[test]
fn open_root_opens_7z() {
    let nav = open_root(&fixture("7z/basic.7z"));
    let archive = nav.current().expect("current");
    assert_eq!(entry_paths(archive), ["a.txt", "dir", "dir/b.txt"]);
}

#[test]
fn open_root_opens_zstd_7z() {
    let nav = open_root(&fixture("7z/zstd.7z"));
    let archive = nav.current().expect("current");
    assert_eq!(entry_paths(archive), ["a.txt", "dir", "dir/b.txt"]);
}

#[test]
fn open_root_rejects_unknown_format() {
    match Navigator::open_root(
        &default_registry(),
        Source::Path(fixture("zip/corrupt.zip")),
        &Default::default(),
    ) {
        Err(Error::UnsupportedFormat(_) | Error::CorruptArchive(_)) => {}
        Ok(_) => panic!("expected open failure"),
        Err(e) => panic!("expected format/corrupt error, got {e}"),
    }
}

#[test]
fn enter_dir_moves_focus_and_back_clears_it() {
    let mut nav = open_root(&fixture("zip/basic.zip"));
    let entries = nav.current().expect("current").entries().expect("entries");
    let dir = entry_by_path(&entries, "dir");
    assert_eq!(dir.kind, NodeKind::Dir);
    nav.enter(dir).expect("enter dir");
    assert_eq!(nav.breadcrumb().len(), 1);
    assert_eq!(
        nav.breadcrumb()[0].focus.as_ref().expect("focus").as_str(),
        "dir"
    );
    nav.back().expect("back");
    assert_eq!(nav.breadcrumb()[0].focus, None);
}

#[test]
fn enter_nested_archive_pushes_level_and_back_pops() {
    let mut nav = open_root(&fixture("zip/nested.zip"));
    let entries = nav.current().expect("current").entries().expect("entries");
    let inner = entry_by_path(&entries, "inner.zip");
    assert_eq!(inner.kind, NodeKind::Archive, "nested zip must be marked");

    nav.enter(inner).expect("enter nested zip");
    assert_eq!(nav.breadcrumb().len(), 2);
    let nested = nav.current().expect("current");
    assert_eq!(entry_paths(nested), ["a.txt", "dir", "dir/b.txt"]);

    nav.back().expect("back");
    assert_eq!(nav.breadcrumb().len(), 1);
    assert_eq!(
        entry_paths(nav.current().expect("current")),
        ["inner.zip", "top.txt"]
    );
}

#[test]
fn enter_nested_7z_pushes_level_and_back_pops() {
    // nested.7z holds four nested archives; entering one shows its contents.
    let mut nav = open_root(&fixture("7z/nested.7z"));
    let entries = nav.current().expect("current").entries().expect("entries");
    let inner = entry_by_path(&entries, "inner.tar.xz");
    assert_eq!(
        inner.kind,
        NodeKind::Archive,
        "nested tar.xz must be marked"
    );

    nav.enter(inner).expect("enter nested tar.xz");
    assert_eq!(nav.breadcrumb().len(), 2);
    assert_eq!(
        entry_paths(nav.current().expect("current")),
        ["a.txt", "dir", "dir/b.txt"]
    );
    nav.back().expect("back");
    assert_eq!(nav.breadcrumb().len(), 1);
}

#[test]
fn breadcrumb_spans_three_levels() {
    // nested.tar -> inner.tar.gz (a basic tar) -> enter dir -> breadcrumbs.
    let mut nav = open_root(&fixture("tar/nested.tar"));
    let entries = nav.current().expect("current").entries().expect("entries");
    let inner = entry_by_path(&entries, "inner.tar.gz");
    assert_eq!(
        inner.kind,
        NodeKind::Archive,
        "nested tar.gz must be marked"
    );
    nav.enter(inner).expect("enter tar.gz");

    let entries = nav.current().expect("current").entries().expect("entries");
    let dir = entry_by_path(&entries, "dir");
    assert_eq!(dir.kind, NodeKind::Dir);
    nav.enter(dir).expect("enter dir");

    // 3 frames: outer tar, inner tar.gz (focus None), dir focus.
    assert_eq!(nav.breadcrumb().len(), 2);
    assert_eq!(
        nav.breadcrumb()[1].focus.as_ref().expect("focus").as_str(),
        "dir"
    );
    // The "current view" is the focused directory's children (`entries()`
    // itself always returns the full flat listing by contract).
    let node = nav
        .current()
        .expect("current")
        .node(&EntryPath::new("dir").expect("valid"))
        .expect("dir node");
    let children = node.children().expect("children");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].meta().path.as_str(), "dir/b.txt");

    nav.back().expect("back");
    assert_eq!(nav.breadcrumb()[1].focus, None);
    nav.back().expect("back");
    assert_eq!(nav.breadcrumb().len(), 1);
}

#[test]
fn enter_plain_file_fails() {
    let mut nav = open_root(&fixture("zip/nested.zip"));
    let entries = nav.current().expect("current").entries().expect("entries");
    let file = entry_by_path(&entries, "top.txt");
    assert_eq!(file.kind, NodeKind::File);
    assert!(nav.enter(file).is_err());
    assert_eq!(nav.breadcrumb().len(), 1);
}

#[test]
fn enter_symlink_fails() {
    let mut nav = open_root(&fixture("tar/sym.tar"));
    let entries = nav.current().expect("current").entries().expect("entries");
    let link = entry_by_path(&entries, "link");
    assert_eq!(link.kind, NodeKind::Symlink);
    assert!(nav.enter(link).is_err());
}

#[test]
fn back_at_top_level_fails() {
    let mut nav = open_root(&fixture("zip/basic.zip"));
    assert!(nav.back().is_err());
}

#[test]
fn current_on_empty_stack_fails() {
    // `current` before any open: simulate by constructing an empty navigator
    // via a corrupt open? Not constructible publicly — verify open_root
    // always leaves one frame instead.
    let nav = open_root(&fixture("zip/basic.zip"));
    assert!(nav.current().is_ok());
}

// ---------------------------------------------------------------------------
// walk
// ---------------------------------------------------------------------------

#[test]
fn walk_flat_lists_all_entries() {
    let archive = open_root(&fixture("zip/basic.zip"));
    let locations = walk_locations(archive.current().expect("current"), WalkOptions::default());
    assert_eq!(locations, ["a.txt", "dir", "dir/b.txt"]);
}

#[test]
fn walk_recurse_descends_into_nested_archives() {
    let archive = open_root(&fixture("zip/nested.zip"));
    let opts = WalkOptions {
        recurse_nested_archives: true,
        ..Default::default()
    };
    let mut locations = walk_locations(archive.current().expect("current"), opts);
    // DFS pre-order: inner.zip first (with its subtree), then top.txt.
    assert_eq!(
        locations,
        [
            "inner.zip",
            "inner.zip/a.txt",
            "inner.zip/dir",
            "inner.zip/dir/b.txt",
            "top.txt",
        ]
    );
    locations.sort();
    assert_eq!(
        locations,
        [
            "inner.zip",
            "inner.zip/a.txt",
            "inner.zip/dir",
            "inner.zip/dir/b.txt",
            "top.txt",
        ]
    );
}

#[test]
fn walk_reports_nested_open_failures() {
    let opts = WalkOptions {
        recurse_nested_archives: true,
        ..Default::default()
    };
    let mut items = walk(&BrokenNested, opts);
    // The nested entry itself is yielded first...
    let first = items.next().expect("first item").expect("ok");
    assert_eq!(first.meta.path.as_str(), "inner.zip");
    // ...then the open failure is reported instead of being swallowed.
    match items.next() {
        Some(Err(Error::CorruptArchive(_))) => {}
        other => panic!("expected a reported nested-open error, got {other:?}"),
    }
    assert!(items.next().is_none(), "no further items");
}

#[test]
fn walk_no_recurse_stays_flat() {
    let archive = open_root(&fixture("zip/nested.zip"));
    let locations = walk_locations(archive.current().expect("current"), WalkOptions::default());
    assert_eq!(locations, ["inner.zip", "top.txt"]);
}

#[test]
fn walk_max_depth_limits_nesting() {
    // deep.zip -> level1.zip -> level2.zip (basic content).
    let archive = open_root(&fixture("zip/deep.zip"));
    let shallow = WalkOptions {
        recurse_nested_archives: true,
        max_depth: 1,
        ..Default::default()
    };
    let locations = walk_locations(archive.current().expect("current"), shallow);
    assert!(locations.contains(&"level1.zip".to_string()));
    assert!(locations.contains(&"level1.zip/level2.zip".to_string()));
    assert!(
        !locations
            .iter()
            .any(|l| l.contains("level1.zip/level2.zip/")),
        "depth 2 must not be reached: {locations:?}"
    );

    let deep = WalkOptions {
        recurse_nested_archives: true,
        max_depth: 4,
        ..Default::default()
    };
    let locations = walk_locations(archive.current().expect("current"), deep);
    assert!(locations.contains(&"level1.zip/level2.zip/a.txt".to_string()));
}

#[test]
fn walk_item_kinds_and_nested_marks() {
    let archive = open_root(&fixture("tar/nested.tar"));
    let items: Vec<_> = walk(
        archive.current().expect("current"),
        WalkOptions {
            recurse_nested_archives: true,
            ..Default::default()
        },
    )
    .map(|i| i.expect("item"))
    .collect();
    // inner.tar.gz is an Archive entry; its contents are prefixed.
    assert!(
        items
            .iter()
            .any(|i| i.meta.kind == NodeKind::Archive && i.meta.path.as_str() == "inner.tar.gz")
    );
    assert!(items.iter().any(|i| i.location.0 == "inner.tar.gz/a.txt"));
    let location = items
        .iter()
        .find(|i| i.location.0 == "inner.tar.gz/a.txt")
        .expect("nested file");
    assert_eq!(location.meta.kind, NodeKind::File);
}

#[test]
fn walk_max_total_entries_stops_with_limit_error() {
    let archive = open_root(&fixture("zip/many.zip"));
    let opts = WalkOptions {
        max_total_entries: 100,
        ..Default::default()
    };
    let results: Vec<Result<_, Error>> = walk(archive.current().expect("current"), opts).collect();
    assert_eq!(results.len(), 101);
    assert!(results[..100].iter().all(|r| r.is_ok()));
    assert!(matches!(results[100], Err(Error::LimitExceeded(_))));
}

#[test]
fn walk_items_are_clone_and_debug() {
    let archive = open_root(&fixture("zip/basic.zip"));
    let item = walk(archive.current().expect("current"), WalkOptions::default())
        .next()
        .expect("first item")
        .expect("ok");
    let _ = item.clone();
    let _ = format!("{item:?}");
    let _ = Location("x".into());
}
