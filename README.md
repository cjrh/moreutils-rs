# moreutils-rs

`moreutils-rs` is a Rust reimplementation of the classic
[moreutils](https://joeyh.name/code/moreutils/) suite: “the unix tools that
nobody thought to write long ago, when unix was young.”

The goal is interface and behaviour compatibility with the original moreutils
where practical, while using idiomatic, memory-safe Rust. This reimplementation
is licensed under **GPL v2 only**; see [`LICENSE`](LICENSE).

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

## Cutting a new release

This project uses [`cargo-release`](https://github.com/crate-ci/cargo-release) to
bump versions, publish to crates.io, and push a tag that triggers the GitHub
Release workflow.

### Prerequisites

Install `cargo-release` once:

```sh
cargo install cargo-release
```

Make sure you are logged into crates.io:

```sh
cargo login
```

### Steps (normal — after initial publish)

1. **Dry run first** — see exactly what will happen without doing anything:

   ```sh
   cargo release <version> --dry-run
   ```

   `<version>` can be a bump level (`patch`, `minor`, `major`) or an explicit
   version like `0.2.0`.

2. **Run the release** — when the dry run looks correct:

   ```sh
   cargo release <version> --execute
   ```

### First-time publish only

crates.io limits new crate publishes to 5 per session, and this workspace has
16 crates. The first release must be done in two stages: bump+tag first, then
publish to crates.io in batches.

1. **Bump, commit, tag, and push — but skip publishing:**

   ```sh
   cargo release patch --no-publish --execute
   ```

   This bumps all versions, creates the git commit and tag, and pushes to
   GitHub (triggering the GitHub Release workflow for binaries).

2. **Publish to crates.io in batches of ≤5.** Wait for each batch to succeed
   before running the next:

   ```sh
   # Batch 1 — common library (must go first; the others depend on it)
   cargo publish -p cjrh-moreutils-common

   # Batch 2
   cargo publish -p cjrh-moreutils-chronic
   cargo publish -p cjrh-moreutils-combine
   cargo publish -p cjrh-moreutils-errno
   cargo publish -p cjrh-moreutils-ifdata

   # Batch 3
   cargo publish -p cjrh-moreutils-ifne
   cargo publish -p cjrh-moreutils-isutf8
   cargo publish -p cjrh-moreutils-lckdo
   cargo publish -p cjrh-moreutils-mispipe

   # Batch 4
   cargo publish -p cjrh-moreutils-parallel
   cargo publish -p cjrh-moreutils-pee
   cargo publish -p cjrh-moreutils-sponge
   cargo publish -p cjrh-moreutils-ts

   # Batch 5
   cargo publish -p cjrh-moreutils-vidir
   cargo publish -p cjrh-moreutils-vipe
   cargo publish -p cjrh-moreutils-zrun
   ```

After this initial publish, all 16 crates exist on crates.io and `cargo release
<version> --execute` will work normally for all future releases — no batching
needed.

### What `cargo release --execute` does

In order:

1. **Bumps versions** across all workspace crates (e.g. `0.1.3` → `0.2.0`),
   including the `cjrh-moreutils-common` workspace dependency version.
2. **Commits** the version changes as a single commit
   (`consolidate-commits = true`).
3. **Publishes to crates.io** in dependency order — `cjrh-moreutils-common`
   first, then all 15 binary crates (`cjrh-moreutils-sponge`, etc.).
4. **Tags** the commit with the bare version number (e.g. `0.2.0`, no `v`
   prefix) as configured by `tag-name = "{{version}}"`.
5. **Pushes** the commit and tag to GitHub.

### What happens on GitHub

The pushed tag triggers the `.github/workflows/release.yml` workflow, which:

1. Builds all 15 binaries as static musl executables
   (`x86_64-unknown-linux-musl`).
2. Packages each binary as `<name>-<version>-x86_64-unknown-linux-musl.tar.gz`.
3. Creates a GitHub Release on the tag and uploads all `.tar.gz` assets.

These assets are what `cargo-binstall` downloads when a user runs:

```sh
cargo binstall cjrh-moreutils-sponge
```

### Crate names vs binary names

Crate names on crates.io use the `cjrh-moreutils-` prefix to avoid collisions
(e.g. `sponge`, `combine`, `parallel`, and `errno` are all taken). Each crate's
`[[bin]]` section ensures the installed binary keeps the original short name:

| Crate name on crates.io        | Binary name installed |
|-------------------------------|-----------------------|
| `cjrh-moreutils-chronic`      | `chronic`             |
| `cjrh-moreutils-combine`      | `combine`             |
| `cjrh-moreutils-errno`        | `errno`               |
| `cjrh-moreutils-ifdata`       | `ifdata`              |
| `cjrh-moreutils-ifne`         | `ifne`                |
| `cjrh-moreutils-isutf8`       | `isutf8`              |
| `cjrh-moreutils-lckdo`        | `lckdo`               |
| `cjrh-moreutils-mispipe`      | `mispipe`             |
| `cjrh-moreutils-parallel`     | `parallel`            |
| `cjrh-moreutils-pee`          | `pee`                 |
| `cjrh-moreutils-sponge`       | `sponge`              |
| `cjrh-moreutils-ts`           | `ts`                  |
| `cjrh-moreutils-vidir`        | `vidir`               |
| `cjrh-moreutils-vipe`         | `vipe`                |
| `cjrh-moreutils-zrun`         | `zrun`                |

So `cargo install cjrh-moreutils-sponge` puts a binary called `sponge` on your
`$PATH`.

### Troubleshooting

- **Publish failed partway through**: Re-run `cargo release` — crates.io
  skips packages that are already published.
- **Tag already exists**: Delete the remote tag and release, bump again, and
  re-run. Or just bump to the next patch version.
- **Version mismatch in workspace dep**: `cargo-release` should update the
  `cjrh-moreutils-common` version in `[workspace.dependencies]` automatically.
  If it doesn't, update it manually before publishing.

## License notes

This repository is GPL-2.0-only. This licence was specifically chosen for
compatibility with the original moreutils code, particularly its GPL-2-only
`sponge` and `parallel` components. See the upstream
[moreutils copyright file](https://metadata.ftp-master.debian.org/changelogs//main/m/moreutils/moreutils_0.70-1_copyright).
