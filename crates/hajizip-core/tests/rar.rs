//! Integration tests for the RAR archive using real fixtures in
//! `testdata/rar/` (provenance and generation commands in
//! `testdata/gen/gen-rar.sh`).
//!
//! Coverage: RAR4 LZSS at every level, RAR4 PPMd, RAR5 levels, RAR5 solid
//! (whole-chain single-member reads), AES-256 content + header encryption
//! (password flow), the accepted rars defect (RAR5 stored + encrypted),
//! multivolume listing, corrupt input, nested rar-in-zip, and registry
//! auto-detection.

use std::io::Read;
use std::path::{Path, PathBuf};

use hajizip_core::archive::rar::RarFormat;
use hajizip_core::registry::Registry;
use hajizip_core::source::Source;
use hajizip_core::{Archive, ArchiveFormat, EntryPath, Error, NodeKind, OpenOptions, Secret};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/rar")
        .canonicalize()
        .expect("testdata/rar must exist (see testdata/gen/gen-rar.sh)")
}

fn fixture(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

fn registry() -> Registry {
    Registry::new().register_archive(RarFormat)
}

fn open(name: &str) -> hajizip_core::Result<Box<dyn Archive>> {
    registry().open_archive(Source::Path(fixture(name)), &OpenOptions::default())
}

fn open_with_password(name: &str, password: &str) -> hajizip_core::Result<Box<dyn Archive>> {
    let opts = OpenOptions {
        password: Some(Secret::new(password)),
        ..Default::default()
    };
    registry().open_archive(Source::Path(fixture(name)), &opts)
}

fn read_entry(archive: &dyn Archive, path: &str) -> hajizip_core::Result<Vec<u8>> {
    let entries = archive.entries()?;
    let entry = entries
        .iter()
        .find(|e| e.path.as_str() == path)
        .expect("entry present");
    let mut reader = archive.reader(entry)?;
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    Ok(buf)
}

fn entry_paths(archive: &dyn Archive) -> Vec<String> {
    let mut paths: Vec<String> = archive
        .entries()
        .expect("listing works")
        .into_iter()
        .map(|e| e.path.as_str().to_owned())
        .collect();
    paths.sort();
    paths
}

/// Known unpacked sizes of the shared text fixture (verified with unrar).
const TEXT_TXT_SIZE: u64 = 2118;

/// A single-file archive: lists one file, reads it back fully.
fn assert_txt_archive(archive: &dyn Archive) {
    let entries = archive.entries().expect("lists");
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.path.as_str(), "text.txt");
    assert_eq!(e.kind, NodeKind::File);
    assert_eq!(e.uncompressed_size, Some(TEXT_TXT_SIZE));
    assert!(e.crc.is_some(), "RAR stores a CRC");
    let bytes = read_entry(archive, "text.txt").expect("reads");
    assert_eq!(bytes.len() as u64, TEXT_TXT_SIZE);
    // reader and extract_to agree byte-for-byte.
    let mut sink = Vec::new();
    let n = archive.extract_to(e, &mut sink).expect("extracts");
    assert_eq!(n as usize, sink.len());
    assert_eq!(sink, bytes);
}

// --- Registry / detection ---------------------------------------------------

#[test]
fn registry_detects_both_rar_signatures() {
    let reg = Registry::with_all_formats();
    assert_eq!(
        reg.detect_archive(b"Rar!\x1a\x07\x00", None)
            .expect("rar4")
            .id(),
        "rar"
    );
    assert_eq!(
        reg.detect_archive(b"Rar!\x1a\x07\x01\x00", None)
            .expect("rar5")
            .id(),
        "rar"
    );
}

#[test]
fn registry_opens_real_rar_by_magic() {
    let reg = Registry::with_all_formats();
    let archive = reg
        .open_archive(
            Source::Path(fixture("rar4-normal.rar")),
            &OpenOptions::default(),
        )
        .expect("opens");
    assert_txt_archive(&*archive);
}

// --- RAR4 LZSS at every level ----------------------------------------------

#[test]
fn rar4_levels_list_and_read() {
    for name in [
        "rar4-fastest.rar",
        "rar4-fast.rar",
        "rar4-normal.rar",
        "rar4-good.rar",
        "rar4-best.rar",
        "rar4-store.rar",
    ] {
        let archive = open(name).unwrap_or_else(|e| panic!("{name} opens: {e}"));
        assert_txt_archive(&*archive);
    }
}

#[test]
fn rar4_archives_report_random_access() {
    for name in ["rar4-fast.rar", "rar4-best.rar"] {
        let archive = open(name).expect("opens");
        assert!(archive.capabilities().random_access, "{name}");
        assert!(!archive.capabilities().encrypted);
        assert!(!archive.capabilities().needs_password);
    }
}

// --- RAR4 PPMd --------------------------------------------------------------

#[test]
fn rar4_ppmd_small_samples_decode() {
    for name in [
        "ppmd-o2.rar",
        "ppmd-o8.rar",
        "ppmd-o16.rar",
        "ppmd-o32.rar",
        "ppmd-o63.rar",
    ] {
        let archive = open(name).unwrap_or_else(|e| panic!("{name} opens: {e}"));
        let entries = archive.entries().expect("lists");
        assert_eq!(entries.len(), 1, "{name}");
        assert_eq!(entries[0].path.as_str(), "text5.txt");
        assert_eq!(entries[0].uncompressed_size, Some(435_000), "{name}");
        let bytes =
            read_entry(&*archive, "text5.txt").unwrap_or_else(|e| panic!("{name} reads: {e}"));
        assert_eq!(bytes.len(), 435_000, "{name}");
    }
}

#[test]
fn rar4_ppmd_large_real_text_decodes() {
    // 14.6 MB of real text, PPMd order 16/31/61.
    for name in ["real-o16.rar", "real-o32.rar", "real-o63.rar"] {
        let archive = open(name).unwrap_or_else(|e| panic!("{name} opens: {e}"));
        let entries = archive.entries().expect("lists");
        assert_eq!(entries[0].path.as_str(), "realtext.txt");
        assert_eq!(entries[0].uncompressed_size, Some(14_631_000));
        let bytes =
            read_entry(&*archive, "realtext.txt").unwrap_or_else(|e| panic!("{name} reads: {e}"));
        assert_eq!(bytes.len(), 14_631_000, "{name}");
    }
}

// --- RAR5 levels and mixed --------------------------------------------------

#[test]
fn rar5_levels_list_and_read() {
    for name in [
        "rar5-fastest.rar",
        "rar5-fast.rar",
        "rar5-normal.rar",
        "rar5-good.rar",
        "rar5-best.rar",
        "rar5-store.rar",
    ] {
        let archive = open(name).unwrap_or_else(|e| panic!("{name} opens: {e}"));
        assert_txt_archive(&*archive);
    }
}

#[test]
fn rar5_mixed_entries_read() {
    let archive = open("rar5-mixed.rar").expect("opens");
    assert_eq!(
        entry_paths(&*archive),
        vec!["photo.jpg".to_string(), "text.txt".to_string()]
    );
    let photo = read_entry(&*archive, "photo.jpg").expect("reads photo");
    assert_eq!(photo.len(), 2_149_083);
    let text = read_entry(&*archive, "text.txt").expect("reads text");
    assert_eq!(text.len() as u64, TEXT_TXT_SIZE);
}

// --- RAR5 solid -------------------------------------------------------------

#[test]
fn rar5_solid_reports_sequential_access_and_reads_members() {
    let archive = open("rar5-solid.rar").expect("opens");
    let caps = archive.capabilities();
    assert!(!caps.random_access, "solid reads decode the whole chain");
    assert_eq!(
        entry_paths(&*archive),
        vec!["photo.jpg".to_string(), "text.txt".to_string()]
    );
    // The second chain member must decode the whole chain to reach it.
    let photo = read_entry(&*archive, "photo.jpg").expect("reads photo");
    assert_eq!(photo.len(), 2_149_083);
    let text = read_entry(&*archive, "text.txt").expect("reads text");
    assert_eq!(text.len() as u64, TEXT_TXT_SIZE);
}

// --- RAR5 encryption (password flow) ---------------------------------------

#[test]
fn content_encryption_requires_password() {
    // Plain header: opens without a password, listing works...
    let archive = open("rar5-enc.rar").expect("opens");
    let caps = archive.capabilities();
    assert!(caps.encrypted);
    assert!(caps.needs_password);
    assert_eq!(
        entry_paths(&*archive),
        vec!["photo.jpg".to_string(), "text.txt".to_string()]
    );
    // ...but reading an encrypted entry reports PasswordRequired.
    let err = read_entry(&*archive, "text.txt").expect_err("must fail");
    assert!(matches!(err, Error::PasswordRequired));

    // With the right password everything decodes.
    let archive = open_with_password("rar5-enc.rar", "test").expect("opens with password");
    let caps = archive.capabilities();
    assert!(caps.encrypted);
    assert!(!caps.needs_password);
    let text = read_entry(&*archive, "text.txt").expect("reads text");
    assert_eq!(text.len() as u64, TEXT_TXT_SIZE);
    let photo = read_entry(&*archive, "photo.jpg").expect("reads photo");
    assert_eq!(photo.len(), 2_149_083);
}

#[test]
fn content_encryption_wrong_password_errors() {
    // rars validates the password while parsing the archive, so opening with
    // a wrong password fails immediately (before any member read).
    let err = open_with_password("rar5-enc.rar", "nope")
        .err()
        .expect("must fail");
    assert!(matches!(err, Error::WrongPassword), "got {err:?}");
}

#[test]
fn encrypted_header_requires_password_on_open() {
    // The header is encrypted: opening without a password fails.
    let err = open("rar5-enc-head.rar").err().expect("must fail");
    assert!(matches!(err, Error::PasswordRequired));
    // With the password, open + list + read all work.
    let archive = open_with_password("rar5-enc-head.rar", "test").expect("opens with password");
    assert_eq!(
        entry_paths(&*archive),
        vec!["photo.jpg".to_string(), "text.txt".to_string()]
    );
    let text = read_entry(&*archive, "text.txt").expect("reads text");
    assert_eq!(text.len() as u64, TEXT_TXT_SIZE);
}

// --- RAR5 stored + encrypted (accepted rars defect) -------------------------

#[test]
fn stored_encrypted_defect_reports_corrupt() {
    // Documented limitation: rars rejects RAR5 stored (m0) + encrypted
    // members with "non-zero padding" even though unrar accepts them. We
    // surface it as CorruptArchive until upstream fixes it (see
    // local-doc/report-rar5-stored-encrypted.md).
    let archive = open_with_password("rar5-store-enc.rar", "test").expect("opens");
    let err = read_entry(&*archive, "text.txt").expect_err("must fail");
    assert!(matches!(err, Error::CorruptArchive(_)), "got {err:?}");
}

// --- Multivolume ------------------------------------------------------------

#[test]
fn multivolume_lists_but_split_members_are_unreadable() {
    // Only the first volume is loaded; split members report UnsupportedFeature
    // (documented limitation, see archive::rar module docs).
    let archive = open("rar4-multi.part1.rar").expect("opens");
    let entries = archive.entries().expect("lists");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.as_str(), "photo.jpg");
    assert_eq!(entries[0].uncompressed_size, Some(2_149_083));
    let err = archive.reader(&entries[0]).err().expect("must fail");
    assert!(matches!(err, Error::UnsupportedFeature(_)), "got {err:?}");
}

// --- Corrupt input ----------------------------------------------------------

#[test]
fn corrupt_data_parse_ok_read_fails() {
    // One data byte flipped: the header parses, but the member CRC fails.
    let archive = open("corrupt.rar").expect("opens (header intact)");
    assert_eq!(entry_paths(&*archive), vec!["text.txt".to_string()]);
    let err = read_entry(&*archive, "text.txt").expect_err("must fail");
    assert!(matches!(err, Error::CorruptArchive(_)), "got {err:?}");
}

#[test]
fn truncated_input_is_rejected() {
    // A 5-byte stub is not a valid RAR archive.
    let fmt = RarFormat;
    let err = fmt
        .open(
            Source::Memory(b"Rar!\x1a".to_vec()),
            &OpenOptions::default(),
        )
        .err()
        .expect("must fail");
    assert!(matches!(err, Error::UnsupportedFormat(_)), "got {err:?}");
}

// --- Nested (rar inside zip) ------------------------------------------------

#[test]
fn rar_inside_zip_opens_as_nested_archive() {
    // nested.zip contains rar4-normal.rar (created by testdata/gen/gen-rar.sh).
    let reg = Registry::with_all_formats();
    let zip = reg
        .open_archive(Source::Path(fixture("nested.zip")), &OpenOptions::default())
        .expect("zip opens");
    let entries = zip.entries().expect("lists");
    let rar_entry = entries
        .iter()
        .find(|e| e.path.as_str() == "rar4-normal.rar")
        .expect("rar entry present");
    // The entry is marked as a nested archive (sniffed at open).
    assert_eq!(rar_entry.kind, NodeKind::Archive);
    let nested = zip
        .open_nested(rar_entry, &OpenOptions::default())
        .expect("rar opens nested");
    assert_txt_archive(&*nested);
}

// --- Node / metadata --------------------------------------------------------

#[test]
fn node_lookup_and_dir_marker() {
    let archive = open("rar5-mixed.rar").expect("opens");
    let node = archive
        .node(&EntryPath::new("text.txt").expect("valid"))
        .expect("node found");
    assert_eq!(node.kind(), NodeKind::File);
    let mut buf = Vec::new();
    node.reader()
        .expect("reads")
        .read_to_end(&mut buf)
        .expect("reads");
    assert_eq!(buf.len() as u64, TEXT_TXT_SIZE);
}

#[test]
fn mtime_is_exposed() {
    // RAR4 members carry a DOS/FAT timestamp; RAR5 samples from WinRAR are
    // created without one (mtime is None there, which is fine too).
    let archive = open("rar4-normal.rar").expect("opens");
    let entries = archive.entries().expect("lists");
    let text = entries
        .iter()
        .find(|e| e.path.as_str() == "text.txt")
        .unwrap();
    let mtime = text.mtime.expect("RAR4 members carry a DOS timestamp");
    // The DOS timestamp decodes to a plausible modern date (the WinRAR sample
    // was created in 2018; exact value is not contractually fixed).
    let secs = mtime
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs();
    assert!(
        (1_500_000_000..1_800_000_000).contains(&secs),
        "unexpected mtime {secs}"
    );
}
