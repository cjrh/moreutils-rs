# moreutils-rs

`moreutils-rs` is a Rust reimplementation of the classic
[moreutils](https://joeyh.name/code/moreutils/) suite: “the unix tools that
nobody thought to write long ago, when unix was young.”

The goal is interface and behaviour compatibility with the original moreutils
where practical, while using idiomatic, memory-safe Rust. This reimplementation
is licensed under **GPL v3 or later**; see [`LICENSE`](LICENSE).

For background on the original tools, see:

- Original moreutils site: <https://joeyh.name/code/moreutils/>
- Original source mirror: <https://github.com/pgdr/moreutils>

## Build

```sh
cargo build --release
```

Binaries are emitted in `target/release/`.

For a development build:

```sh
cargo build
```

## Compatibility checks

If the original moreutils package is installed in `/bin`, run:

```sh
tests/compat-smoke.sh
```

The smoke tests compare representative behaviours against the installed tools.
They are not exhaustive; more edge-case compatibility tests are welcome.

## Tools

### `chronic` — run quietly unless something fails

Useful for cron jobs: keep successful runs silent, but preserve diagnostics on
failure.

```sh
0 1 * * * chronic rsync -a /srv/ backup:/srv/
```

Debug with verbose output:

```sh
chronic -v ./nightly-maintenance
```

Treat stderr output as failure even if the command exits successfully:

```sh
chronic -e ./script-that-should-not-warn
```

### `combine` — boolean operations on lines from two files

Find lines present in both files:

```sh
combine wanted.txt and available.txt
```

Find items in one file but not another:

```sh
combine all-hosts.txt not retired-hosts.txt
```

Other operations are `or` and `xor`.

### `errno` — look up errno names, numbers, and descriptions

```sh
errno ENOENT
errno 13
errno -s permission denied
```

List all known errno values:

```sh
errno --list
```

### `ifdata` — script-friendly network interface information

Get the IPv4 address of an interface without parsing `ifconfig` output:

```sh
ifdata -pa eth0
```

Check whether an interface exists:

```sh
ifdata -e wg0 && echo "wireguard interface exists"
```

Print the interface MTU:

```sh
ifdata -pm eth0
```

### `ifne` — run a command only if stdin is non-empty

Send mail only when `find` produces results:

```sh
find /var/crash -type f | ifne mail -s "Crash files found" root
```

Reverse the test with `-n`: run the command only when stdin is empty, otherwise
pass input through unchanged.

```sh
printf '%s\n' "$maybe_empty" | ifne -n echo "no input"
```

### `isutf8` — validate UTF-8 input

Check files and print diagnostics for invalid UTF-8:

```sh
isutf8 *.txt
```

Use quiet mode in scripts:

```sh
if isutf8 -q data.json; then
    jq . data.json
fi
```

List only invalid files:

```sh
isutf8 --list **/*
```

### `lckdo` — run a program while holding a lock

Prevent overlapping jobs:

```sh
lckdo -w /tmp/backup.lock ./backup
```

Wait up to 30 seconds for the lock:

```sh
lckdo -W 30 /tmp/import.lock ./import-new-data
```

Note: like upstream moreutils, `lckdo` is mostly superseded by `flock(1)`.

### `mispipe` — pipe commands but return the first command’s status

Useful when the second command is a logger or formatter and the first command’s
exit status is the one that matters.

```sh
mispipe './build.sh' 'logger -t nightly-build'
```

Unlike shell pipelines, `mispipe` returns the status of `./build.sh`.

### `parallel` — run many jobs concurrently

Run one job per CPU by default:

```sh
parallel gzip -- *.log
```

Limit concurrency:

```sh
parallel -j 4 convert -- *.png
```

Replace `{}` inside the command with `-i`:

```sh
parallel -i sh -c 'mkdir -p "out/$(dirname {})"; cp "{}" "out/{}"' -- $(find src -type f)
```

Run independent shell commands in parallel:

```sh
parallel -j 3 -- 'make test' 'cargo test' 'npm test'
```

### `pee` — tee stdin to commands instead of files

Feed the same stream to multiple commands:

```sh
journalctl -f | pee 'grep -i error' 'grep -i warning'
```

Unlike `tee`, `pee` does not copy stdin to stdout unless you ask it to:

```sh
producer | pee cat 'sha256sum > stream.sha256' | consumer
```

### `sponge` — soak up stdin before writing a file

Safely transform a file in place without truncating it before the pipeline has
read it:

```sh
grep -v '^#' config | sponge config
```

Append atomically when possible:

```sh
printf '%s\n' "new setting" | sponge -a config
```

If no output file is supplied, `sponge` writes to stdout.

### `ts` — timestamp each input line

Prefix log lines with wall-clock timestamps:

```sh
make test 2>&1 | ts
```

Use a custom `strftime`-style format:

```sh
ping example.com | ts '%Y-%m-%dT%H:%M:%S%z'
```

Measure incremental time between lines:

```sh
long-running-command | ts -i '%.S'
```

### `vidir` — edit a directory listing in your editor

Rename or delete files by editing a generated list:

```sh
vidir ~/Downloads
```

Typical use: open the directory listing, change filenames in the editor, save,
and exit. Removed lines delete files; changed names rename files.

### `vipe` — insert an editor into a pipeline

Pause a pipeline for manual editing:

```sh
generate-config | vipe | deploy-config
```

Use with `$EDITOR`/`$VISUAL` to quickly patch generated text before continuing.

### `zrun` — transparently decompress command arguments

Run a command on compressed files that the command itself does not understand:

```sh
zrun grep -n ERROR app.log.gz old-app.log.xz
```

If invoked through a symlink whose name starts with `z`, such as `zgrepplain`, it
acts as `zrun grepplain ...`.

## Workspace layout

Each tool is a separate binary crate under `crates/`, with shared helpers in
`crates/common`.

```text
crates/
  chronic/
  combine/
  errno/
  ifdata/
  ifne/
  isutf8/
  lckdo/
  mispipe/
  parallel/
  pee/
  sponge/
  ts/
  vidir/
  vipe/
  zrun/
```

## License notes

This repository is GPL-3.0-or-later. The original moreutils project is
GPL-family software; this project is a Rust reimplementation based on the public
interfaces and compatibility testing against installed moreutils binaries.
