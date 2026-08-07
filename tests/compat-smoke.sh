#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-only
set -euo pipefail

cargo build >/dev/null
BIN="$(pwd)/target/debug"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

compare() {
  local name="$1"; shift
  set +e
  "$@" >"$TMP/$name.ours.out" 2>"$TMP/$name.ours.err"
  local ours_rc=$?
  set -e
  local orig=("/bin/${1##*/}" "${@:2}")
}

# combine
printf 'a\nb\na\nc\n' >"$TMP/one"
printf 'b\nd\nb\n' >"$TMP/two"
for op in and not or xor; do
  /bin/combine "$TMP/one" "$op" "$TMP/two" >"$TMP/orig"
  "$BIN/combine" "$TMP/one" "$op" "$TMP/two" >"$TMP/ours"
  diff -u "$TMP/orig" "$TMP/ours"
done

# sponge
printf 'old\n' >"$TMP/sponge-file"
printf 'new\n' | "$BIN/sponge" "$TMP/sponge-file"
test "$(cat "$TMP/sponge-file")" = "new"
printf 'plus\n' | "$BIN/sponge" -a "$TMP/sponge-file"
test "$(cat "$TMP/sponge-file")" = $'new\nplus'

# ifne
printf 'hello' | "$BIN/ifne" cat >"$TMP/ifne-out"
test "$(cat "$TMP/ifne-out")" = "hello"
printf '' | "$BIN/ifne" sh -c 'exit 9'
printf 'hello' | "$BIN/ifne" -n cat >"$TMP/ifne-n-out"
test "$(cat "$TMP/ifne-n-out")" = "hello"

# chronic
set +e
/bin/chronic -v sh -c 'echo out; echo err >&2; exit 3' >"$TMP/chronic.orig.out" 2>"$TMP/chronic.orig.err"; orig_rc=$?
"$BIN/chronic" -v sh -c 'echo out; echo err >&2; exit 3' >"$TMP/chronic.ours.out" 2>"$TMP/chronic.ours.err"; ours_rc=$?
set -e
test "$orig_rc" = "$ours_rc"
diff -u "$TMP/chronic.orig.out" "$TMP/chronic.ours.out"
diff -u "$TMP/chronic.orig.err" "$TMP/chronic.ours.err"

# mispipe
set +e
"$BIN/mispipe" 'printf x; exit 7' 'cat >/dev/null'
test "$?" = 7
set -e

# pee
printf abc | "$BIN/pee" 'wc -c' 'wc -c' | sort >"$TMP/pee"
test "$(cat "$TMP/pee")" = $'3\n3'

# isutf8
printf 'ok\n' >"$TMP/utf8-good"
printf 'bad\xff\n' >"$TMP/utf8-bad"
"$BIN/isutf8" "$TMP/utf8-good"
set +e
"$BIN/isutf8" -q "$TMP/utf8-bad"
test "$?" = 1
set -e

# errno
/bin/errno ENOENT >"$TMP/errno.orig"
"$BIN/errno" ENOENT >"$TMP/errno.ours"
diff -u "$TMP/errno.orig" "$TMP/errno.ours"

# ifdata stable checks
/bin/ifdata -pe lo >"$TMP/ifdata.orig"
"$BIN/ifdata" -pe lo >"$TMP/ifdata.ours"
diff -u "$TMP/ifdata.orig" "$TMP/ifdata.ours"

printf 'compat smoke tests passed\n'
