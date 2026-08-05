#!/usr/bin/env bash
# Generate RAR test fixtures under testdata/rar/.
#
# Most fixtures are WinRAR / RAR 4.20 samples that cannot be reproduced here
# (WinRAR is proprietary; the RAR 4.20 static binary is not redistributable).
# Their provenance and the exact source commands are recorded below so the
# fixtures can be re-created from scratch if needed.
#
# Sources:
#   1. WinRAR samples (RAR4 -m1..-m5/-m0, RAR5 -m1..-m5/-m0, solid, AES-256
#      with password "test", encrypted header, multivolume): downloaded from
#      the GitHub release assets of Roba1993/RAR (mirrored in /tmp/rar-github/).
#      `rar a -mX archive.rar text.txt photo.jpg` style commands, password
#      `-ptest`, header encryption `-hp`, solid `-s`, volumes `-v1m`.
#   2. RAR4 PPMd samples: generated with the RAR 4.20 static binary
#      (`rar_static`, from the official RAR 4.20 distribution):
#        ./rar_static a -m5 -mc<order>:<mem>t+ out.rar text.txt
#      The `t+` suffix is required to select the PPMd (text) module; orders
#      and dictionaries used:
#        ppmd-o2  : -mc2:1mt+   (order 2, 1 MB)   on text5.txt
#        ppmd-o8  : -mc8:4mt+   (order 8, 4 MB)
#        ppmd-o16 : -mc16:8mt+  (order 16, 8 MB)
#        ppmd-o32 : -mc31:4mt+  (order 31, 4 MB)
#        ppmd-o63 : -mc61:32mt+ (order 61, 32 MB)
#        real-o16 : -mc16:128mt+ on realtext.txt (14.6 MB)
#        real-o32 : -mc31:4mt+   on realtext.txt
#        real-o63 : -mc61:32mt+  on realtext.txt
#   3. corrupt.rar is derived from rar4-normal.rar by flipping one data byte
#      (recreated below), so it is reproducible without any RAR tool.
#
# The PPMd source texts (text5.txt / realtext.txt) are NOT committed (the
# 14.6 MB realtext.txt would bloat the repo); only the small archives are.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUTDIR="$SCRIPT_DIR/../rar"
mkdir -p "$OUTDIR"
cd "$OUTDIR"

# --- corrupt.rar: one data byte flipped in rar4-normal.rar -------------------
# (parse succeeds, member read fails the CRC check)
if [[ -f rar4-normal.rar ]]; then
    cp rar4-normal.rar corrupt.rar
    SZ=$(stat -c%s rar4-normal.rar)
    printf '\xff' | dd of=corrupt.rar bs=1 seek=$((SZ / 2)) conv=notrunc status=none
    echo "regenerated corrupt.rar from rar4-normal.rar (byte $((SZ / 2)))"
fi

# --- nested.zip: rar4-normal.rar inside a zip (nested-archive navigation) -----
# Requires the Info-ZIP `zip` CLI. `-X` drops extra attributes and `-t` pins the
# entry timestamp so the fixture is byte-stable across regenerations.
if [[ -f rar4-normal.rar ]] && command -v zip >/dev/null; then
    (cd "$OUTDIR" && rm -f nested.zip && zip -q -j -X -t 2024-01-01 nested.zip rar4-normal.rar)
    echo "regenerated nested.zip (contains rar4-normal.rar)"
fi