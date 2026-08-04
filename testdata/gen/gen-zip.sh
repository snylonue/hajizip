#!/usr/bin/env bash
# Generate zip test fixtures under testdata/zip/ using Info-ZIP `zip 3.0`.
#
# Reproducible: `zip -X` strips extra attributes so output depends only on
# input bytes and the zip version. Regenerate with `zip 3.0` (Info-ZIP).
# Intermediate build trees live in a temp dir and are removed on exit.
set -euo pipefail

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Zero source-file mtimes so `zip` records a fixed timestamp and output is
# byte-stable across regenerations.
touch_epoch() {
    find "$1" -exec touch -d '@0' {} +
}

OUT="$(cd "$(dirname "$0")/../zip" && pwd)"
cd "$OUT"
rm -f ./*.zip
rm -rf build build2 build3 build4 build5

mkdir -p "$TMP/build/dir"
printf 'Hello, hajizip!\n' > "$TMP/build/a.txt"
printf 'nested content\n' > "$TMP/build/dir/b.txt"
mkdir -p "$TMP/build2"
printf 'utf8 content\n' > "$TMP/build2/你好.txt"
printf 'ascii\n' > "$TMP/build2/hello.txt"

# --- basic.zip: deflate + explicit dir entry -------------------------------
touch_epoch "$TMP/build"
(cd "$TMP/build" && zip -X -r -q "$OUT/basic.zip" a.txt dir)

# --- stored.zip: stored (no compression) ------------------------------------
(cd "$TMP/build" && zip -X -0 -q "$OUT/stored.zip" a.txt)

# --- utf8.zip: UTF-8 names with the EFS flag set ----------------------------
touch_epoch "$TMP/build2"
(cd "$TMP/build2" && zip -X -q "$OUT/utf8.zip" 你好.txt hello.txt)

# --- gbk.zip: GBK-encoded name, EFS NOT set (byte-patch trick) --------------
# Create a single-file zip named "xxxx" (4 ASCII bytes), then replace the
# name with the GBK bytes of "你好" (C4 E3 BA C3 — also 4 bytes) in both the
# local header and the central directory entry. Equal lengths mean no other
# field needs adjusting.
printf 'gbk content\n' > "$TMP/build2/xxxx"
touch_epoch "$TMP/build2"
(cd "$TMP/build2" && zip -X -q "$OUT/gbk-raw.zip" xxxx)
size=$(stat -c%s "$OUT/gbk-raw.zip")
cd_off=$(od -An -tu4 --endian=little -j $((size - 6)) -N4 "$OUT/gbk-raw.zip" | tr -d ' ')
cp "$OUT/gbk-raw.zip" "$OUT/gbk.zip"
printf '\xc4\xe3\xba\xc3' | dd of="$OUT/gbk.zip" bs=1 seek=30 count=4 conv=notrunc 2>/dev/null
printf '\xc4\xe3\xba\xc3' | dd of="$OUT/gbk.zip" bs=1 seek=$((cd_off + 46)) count=4 conv=notrunc 2>/dev/null
rm -f "$OUT/gbk-raw.zip"

# --- zipslip.zip: entry with a `..` path component + a legit file -----------
mkdir -p "$TMP/build3/outer"
printf 'evil\n' > "$TMP/build3/evil.txt"
printf 'ok\n' > "$TMP/build3/ok.txt"
# Info-ZIP keeps the `..` component when the file is referenced through one.
touch_epoch "$TMP/build3"
(cd "$TMP/build3" && zip -X -q "$OUT/zipslip.zip" outer/../evil.txt ok.txt)

# --- enc.zip: ZipCrypto-encrypted entry -------------------------------------
# NOTE: ZipCrypto embeds a random salt, so enc.zip bytes are NOT reproducible
# across regenerations; tests assert behaviour (encrypted flag / read refusal),
# not bytes.
touch_epoch "$TMP/build"
(cd "$TMP/build" && zip -X -q -P hunter2 "$OUT/enc.zip" a.txt)

# --- nested.zip: zip containing another zip ---------------------------------
mkdir -p "$TMP/build4"
cp "$OUT/basic.zip" "$TMP/build4/inner.zip"
printf 'top\n' > "$TMP/build4/top.txt"
touch_epoch "$TMP/build4"
(cd "$TMP/build4" && zip -X -q "$OUT/nested.zip" inner.zip top.txt)

# --- deep.zip: two levels of nested zips -------------------------------------
mkdir -p "$TMP/deep"
cp "$OUT/basic.zip" "$TMP/deep/level2.zip"
touch_epoch "$TMP/deep"
(cd "$TMP/deep" && zip -X -q "$OUT/level1.zip" level2.zip)
cp "$OUT/level1.zip" "$TMP/deep/level1.zip"
touch_epoch "$TMP/deep"
(cd "$TMP/deep" && zip -X -q "$OUT/deep.zip" level1.zip)

# --- sjis.zip: Shift-JIS names, EFS NOT set -------------------------------
# Raw SJIS bytes are written directly into filenames (bash $'\x..' quoting) and
# `LANG=C zip` stores them verbatim without the UTF-8 flag. Names:
#   ウラレタウン.exe (16B), 説明.txt (8B), 本.txt (6B) — the shorter two are
# too short for chardetng individually; the aggregate is what the Auto
# strategy detects with (see crates/hajizip-core/src/encoding.rs).
mkdir -p "$TMP/build6"
printf 'content-a\n' > "$TMP/build6/$(printf '\x83\x45\x83\x89\x83\x8c\x83\x5e\x83\x45\x83\x93.exe')"
printf 'content-b\n' > "$TMP/build6/$(printf '\x90\xe0\x96\xbe.txt')"
printf 'content-c\n' > "$TMP/build6/$(printf '\x96\x7b.txt')"
touch_epoch "$TMP/build6"
(cd "$TMP/build6" && LANG=C zip -X -0 -q "$OUT/sjis.zip" *)

# --- corrupt.zip: truncated in the middle of the central directory ----------
head -c 64 basic.zip > corrupt.zip

# --- empty.zip: valid empty archive (22-byte EOCD) --------------------------
printf '\x50\x4b\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00' > empty.zip

# --- many.zip: 10 000 small entries -----------------------------------------
mkdir -p "$TMP/build5"
for i in $(seq 1 10000); do printf x > "$TMP/build5/f$i"; done
touch_epoch "$TMP/build5"
(cd "$TMP/build5" && zip -X -q "$OUT/many.zip" f*)

# --- manifest -----------------------------------------------------------------
cat > manifest.toml <<'EOF'
# Zip fixtures. Generated by testdata/gen/gen-zip.sh (Info-ZIP zip 3.0, -X).
# `name` lists the raw entry names (unzip -Z1 output).

[basic.zip]
name = ["a.txt", "dir/", "dir/b.txt"]

[stored.zip]
name = ["a.txt"]
note = "stored (no compression) method"

[utf8.zip]
name = ["你好.txt", "hello.txt"]
note = "UTF-8 names with EFS flag set"

[gbk.zip]
name = ["\u{fffd}\u{fffd}\u{fffd}\u{fffd}"]
note = "name bytes are GBK C4 E3 BA C3 (你好), EFS unset; lossy-decoded"

[sjis.zip]
name = ["\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}.exe", "\u{fffd}\u{fffd}\u{fffd}\u{fffd}.txt", "\u{fffd}\u{fffd}.txt"]
note = "name bytes are Shift-JIS (ウラレタウン.exe / 説明.txt / 本.txt), EFS unset; Auto strategy detects Shift-JIS from the aggregate"

[zipslip.zip]
name = ["outer/../evil.txt", "ok.txt"]
note = "entry with `..` component must be skipped at listing"

[enc.zip]
name = ["a.txt"]
note = "ZipCrypto-encrypted (password hunter2); reads unsupported in M1"

[nested.zip]
name = ["inner.zip", "top.txt"]
note = "inner.zip is a copy of basic.zip"

[deep.zip]
name = ["level1.zip"]
note = "level1.zip contains level2.zip (a copy of basic.zip): 2 nested levels"

[corrupt.zip]
note = "truncated central directory; open must fail"

[empty.zip]
name = []
note = "valid empty archive"

[many.zip]
name_count = 10000
note = "10 000 small entries"
EOF

echo "generated:"
ls -l ./*.zip
