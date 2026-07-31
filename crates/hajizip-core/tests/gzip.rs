//! Integration tests for the gzip codec using real fixtures in `testdata/gzip/`.
//!
//! Expected payloads are byte-compared against the plaintext fixture files
//! (single source of truth), rather than hardcoding hashes in this file.
//! `testdata/gzip/manifest.toml` documents sizes and sha256 for humans/tools.

use std::io::Read;
use std::path::{Path, PathBuf};

use hajizip_core::Codec;
use hajizip_core::codec::gzip::{GzipCodec, GzipFormat};
use hajizip_core::registry::Registry;
use hajizip_core::source::Source;

/// Absolute path of the gzip fixture directory.
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/gzip")
        .canonicalize()
        .expect("testdata/gzip must exist (run testdata/gen/gen-gzip.sh)")
}

fn fixture(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

/// Decompress a whole fixture file.
fn decompress_file(name: &str) -> hajizip_core::Result<Vec<u8>> {
    let bytes = std::fs::read(fixture(name)).expect("fixture readable");
    let codec = GzipCodec;
    let mut reader = codec.decompress(Box::new(bytes.as_slice()))?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

#[test]
fn hello_single_member_matches_plaintext() {
    let out = decompress_file("hello.txt.gz").expect("decompression succeeds");
    let expected = std::fs::read(fixture("hello.txt")).expect("plaintext readable");
    assert_eq!(out, expected);
}

#[test]
fn multi_member_decodes_all_members() {
    let out = decompress_file("multi-member.gz").expect("decompression succeeds");
    let mut expected = std::fs::read(fixture("hello.txt")).expect("plaintext readable");
    expected.extend(std::fs::read(fixture("world.txt")).expect("plaintext readable"));
    assert_eq!(out, expected);
}

#[test]
fn truncated_input_errors() {
    let err = decompress_file("corrupt-truncated.gz").expect_err("must fail");
    assert!(matches!(err, hajizip_core::error::Error::Io(_)));
}

#[test]
fn corrupt_crc_errors() {
    let err = decompress_file("corrupt-crc.gz").expect_err("must fail");
    assert!(matches!(err, hajizip_core::error::Error::Io(_)));
}

#[test]
fn registry_detects_gzip_by_magic() {
    let reg = Registry::new().register_codec(GzipFormat);
    let head = std::fs::read(fixture("hello.txt.gz")).expect("fixture readable");
    let fmt = reg
        .detect_codec(&head, None)
        .expect("gzip magic must be detected");
    assert_eq!(fmt.id(), "gzip");
}

#[test]
fn registry_detects_gzip_by_extension() {
    let reg = Registry::new().register_codec(GzipFormat);
    let fmt = reg
        .detect_codec(b"not a gzip", Some("gz"))
        .expect("extension fallback must hit");
    assert_eq!(fmt.id(), "gzip");
}

#[test]
fn bare_gzip_is_not_an_archive() {
    // A standalone `.gz` is a single stream, not a file tree. Until the
    // codec+archive composition (e.g. tar.gz) lands, opening it as an archive
    // must fail cleanly with UnsupportedFormat (no archive format registered).
    let reg = Registry::new().register_codec(GzipFormat);
    let src = Source::Path(fixture("hello.txt.gz"));
    match reg.open_archive(src, &Default::default()) {
        Err(hajizip_core::error::Error::UnsupportedFormat(_)) => {}
        Ok(_) => panic!("expected UnsupportedFormat, got Ok"),
        Err(e) => panic!("expected UnsupportedFormat, got {e}"),
    }
}
