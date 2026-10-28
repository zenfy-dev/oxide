# Architecture

```text
crates/
├── oxide-core   archive engine (library). Knows nothing about either frontend.
├── oxide-cli    `oxide` binary: clap → oxide-core, progress via indicatif
└── oxide-gui    `oxide-gui` binary: egui/eframe → oxide-core on worker threads
```

Both frontends call the same public functions, and neither runs the other as a subprocess.

| Function | Purpose |
|---|---|
| `detect_format(path)` | Signature-based detection |
| `list(path, &ReadOptions)` / `list_with_progress` | Read the entry index |
| `test(path, &ReadOptions, &Progress)` | Decode everything and verify checksums, writing nothing |
| `extract(path, dest, &ExtractOptions, &Progress)` | Safe extraction |
| `create(sources, output, &CreateOptions, &Progress)` | New archive |
| `add(archive, sources, &CreateOptions, &Progress)` | Rewrite an archive with extra files (ZIP, TAR family) |
| `supported_formats()` | Capabilities of this build |

Every operation returns `Result<_, OxideError>`. Its variants are coarse on purpose
(`Corrupt`, `PasswordRequired`, `UnsafePath`, `LimitExceeded`, `Cancelled`, ...), so frontends
can map them to messages and exit codes without knowing format internals.

## Core modules

| Module | Responsibility |
|---|---|
| `formats.rs` | `ArchiveFormat`, `CompressionMethod`, capabilities (`FormatInfo`), signature detection |
| `archive.rs` | `Entry`, `ArchiveInfo`, the `FormatHandler` and `EntryVisitor` traits, `list`/`test`/`add` |
| `extraction.rs` | `ExtractOptions`, path sanitising, `ExtractVisitor` (all extraction safety policy) |
| `compression.rs` | `CreateOptions`, input collection, `CreateJob`, stream codecs (`Codec`) |
| `encryption.rs` | `Password` (zeroized, redacted `Debug`), `EncryptionMethod` |
| `progress.rs` | `Progress`, `CancelToken`, throttled `ProgressUpdate`s, internal `Tracker` |
| `handlers/` | One module per format: `zip`, `tar` (+ codecs), `stream` (single-file), `sevenz`, `rar` |

### Handlers and visitors

```text
           ┌──────────── list ─────────────┐
 handler ──┤                               ├── EntryVisitor ──┬─ ExtractVisitor (writes safely)
 .read()   └─ yields (Entry, Option<Read>) ┘                  ├─ TestVisitor (drains, verifies)
                                                              └─ Collect (listing)
```

A `FormatHandler` only knows how to walk its format. It produces an `Entry` (with the name
**exactly as stored**) and, for regular files, a `Read` of the decoded data. Everything that
matters for safety lives in the visitors, so every format gets identical treatment:

- `wants(entry)` handles selection and fail-fast limit checks on declared sizes. Random-access
  formats (ZIP, 7z) skip decoding unwanted entries entirely.
- `visit(entry, data)` sanitises the path, applies overwrite policy, streams data while counting
  real bytes against limits, writes to a temp file and renames it into place.
- `finish()` creates deferred symlinks, then re-verifies them by resolving the real paths.

ZIP indexing uses the in-memory central directory, without seeking to individual local
headers. Extraction selections use a hash set with ancestor lookups, so matching cost
depends on path depth rather than the number of selected files. `strip_components` removes
leading components only after path validation; selection matches original archive paths,
hard-link targets are stripped too, and output collisions follow the overwrite policy.
`exact_selection` instead matches raw stored names without descendant or `./` normalization,
allowing one regular file to be prepared without extracting similarly named entries.

Creation is the mirror image. `CreateJob::prepare` validates options, walks inputs in sorted
order (excluding the output itself), and records sizes. Handlers then write entries, using
`job.open(item)`, a reader that reports progress, honours cancellation, and errors if a file
changes size mid-read. `create()` writes into a `NamedTempFile` in the destination directory
and persists it only on success (`persist_noclobber` unless overwriting).

Parallel ZIP compression keeps at most two pending files per worker and 64 MiB of input
bytes across queued, compressing and completed files together. Results count against both
limits until written, preventing a slow early file from retaining unbounded later results.
Compression workers reuse their read buffers.

### Progress and cancellation

`Progress` carries an optional callback and a `CancelToken`. Internally a `Tracker`
(single-threaded, `RefCell`) accumulates counters and calls the callback at most every 50 ms.
Completion is estimated from uncompressed bytes when totals are known (ZIP, 7z, create), and
from compressed bytes read otherwise (TAR streams). `CountingReader` reports the latter and
also checks for cancellation, so cancelling interrupts even a long skip over one huge entry.

Cancellation is cooperative: it is checked between entries, on every read chunk, and inside
reader wrappers handed to third-party libraries. When a library wraps our error in its own
type, the top-level operation checks the token and reports `Cancelled`.

## GUI threading model

```text
UI thread (egui)                     worker thread
────────────────                     ─────────────
spawn(kind, closure) ─────────────▶  oxide_core::extract(..., &progress)
  ▲  reads JobShared.update   ◀──── progress callback: store snapshot + request_repaint()
  │  cancel button → CancelToken ──▶ checked by core
  └─ poll_job(): JobShared.outcome ◀─ result stored + request_repaint()
```

At most one job runs at a time. `PasswordRequired`/`WrongPassword` raise a prompt and re-run
the last `Action` with the new password. Panics in a worker are caught and reported as errors.

The GUI is split into `app.rs` (state, actions, jobs), `ui.rs` (layout, toolbar, modals,
shortcuts, drag-and-drop), `theme.rs` (visuals and the embedded app icon) and
`components/` (file table and folder tree, create/extract dialogs, progress modal).
`devtools.rs` exists only in debug builds and lets the app screenshot itself for documentation.

The browser caches archive totals and lowercase search paths when opening an archive.
Multiword searches match every term against the cached full path, and natural sorting
compares numeric slices without allocating strings for each comparison.

`file_access.rs` prepares an exact regular-file selection through the core extractor in a
private temporary directory. Extraction, bounded text/image decoding, and the OS default-app
handoff run on worker threads with the existing progress, cancellation and password retry
flow. Links cannot be opened. `components/preview.rs` renders a read-only text or image window.
Temporary directories use shared leases: closing a preview releases its copy, while files
handed to another app remain alive until Oxide exits. Opening a file never updates the archive.

## Adding a format

1. **Declare it.** Add a variant to `ArchiveFormat` in `formats.rs`, then fill in `name()`,
   `extensions()`, `info()` (extract/create/encryption/methods), and its magic bytes in
   `detect_signature`. If it is optional, gate it behind a Cargo feature in
   `crates/oxide-core/Cargo.toml` and report `can_extract: cfg!(feature = "...")`.
2. **Implement `FormatHandler`** in `handlers/<name>.rs`:
   - `read(ctx, visitor)`: call `visitor.begin(Some(&index))` if you know the entry list up
     front, otherwise `begin(None)`. For each entry build an `Entry` with the raw stored name,
     call `visitor.wants(&entry)?`, then `visitor.visit(&entry, Some(&mut reader))` for files
     or `None` otherwise. Put symlink targets in `entry.link_target`. Call
     `ctx.tracker.check_cancel()?` per entry, and wrap the file reader in
     `CountingReader::new(file, ctx.tracker)` for streaming progress. **Do not** sanitise
     paths or create files yourself. The visitor does that.
   - `list(ctx)`: optional fast path (the default walks `read` without decoding).
   - `create(out, job)`: iterate `job.inputs`, calling `job.tracker.start_entry` and
     `finish_entry`. Read file data through `job.open(item)` or `job.copy_into(item, w)`.
   - `append(ctx, out, job)`: optional. Copy existing entries and then add `job.inputs`.
   - Map library errors: data problems become `OxideError::Corrupt`, password failures become
     `PasswordRequired`/`WrongPassword`.
3. **Register it** in `handlers::handler_for`.
4. **Test it.** Add the format to `MULTI_FILE` in `tests/roundtrip.rs` (if writable), add
   corruption and truncation cases to `tests/security.rs`, and update `docs/FORMATS.md`. The
   CLI and GUI pick the format up automatically from `supported_formats()`.

## Dependencies

| Crate | Why |
|---|---|
| `zip` 9 | ZIP read/write incl. Zip64, AES, Deflate/Bzip2/Zstd/XZ |
| `tar` 0.4 | TAR read/write (GNU long names, PAX, sparse) |
| `flate2`, `bzip2`, `liblzma`, `zstd` | Codecs. `liblzma` is the maintained fork of `xz2` and is built statically. |
| `sevenz-rust2` | Pure-Rust 7z with LZMA2, AES-256, multithreaded encoding |
| `unrar` (optional) | RARLAB UnRAR bindings; freeware licence, so opt-in |
| `walkdir`, `tempfile`, `zeroize`, `thiserror`, `tracing`, `serde`, `chrono` | Infrastructure |
| `clap`, `indicatif`, `ctrlc` | CLI |
| `eframe`/`egui`, `egui_extras`, `rfd` | GUI, table, native dialogs (XDG portal on Linux) |
| `image`, `open` | Bounded local image previews and OS default-application handoff |
