#!/usr/bin/env bash
# Generate the CP437 -> Unicode code point table (256 entries) using the
# system iconv (glibc). Output: one decimal code point per line, byte 0x00
# first. The table is embedded in crates/hajizip-core/src/encoding.rs.
#
# CP437 is not part of the WHATWG encoding set (encoding_rs), so the project
# ships its own mapping; this script documents how it was produced.
set -euo pipefail

for i in $(seq 0 255); do
    # Escape the byte as \xNN directly in printf's format string and pipe it
    # straight into iconv (never through a shell variable: NUL bytes would be
    # truncated). `od` already emits the trailing newline.
    printf "\\x$(printf '%02x' "$i")" | iconv -f CP437 -t UTF-32LE 2>/dev/null |
        od -An -tu4 | tr -d ' '
done
