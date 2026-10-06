# PST Searcher

A small, self-contained Windows desktop app that **indexes and searches PST files**
(Outlook exports, Microsoft Purview eDiscovery / content-search bundles) and exports
individual messages. Read-only: it never sends or receives mail, and it needs no
Outlook, no Office, no mail server, and no runtime installed.

**Status:** working prototype, verified running on Windows 11 (gaming PC) and Linux.
Reads real PST files, indexes into SQLite FTS5, searches with BM25 ranking, exports `.eml`.

## What is verified

| Item | Result |
|---|---|
| Parser | Microsoft's clean-room Rust crate `outlook-pst` (MIT, MS-PST spec) |
| Search engine | SQLite 3.46.0 with `ENABLE_FTS5` compiled in (rusqlite `bundled`) |
| Message class (`IPM.Note` / `IPM.Contact`) | read directly from `PR_MESSAGE_CLASS` (0x001A) |
| Dedupe | item + occurrence tables; duplicates collapse, every source PST retained |
| Sizes | CLI 1.44 MB · GUI (egui) 10.0 MB · GUI working set ~190 MB with CJK font loaded |
| Platform | built on Linux, cross-compiled to a Windows PE32+ exe, no toolchain on the PC |

## Browsing / loading PSTs (GUI)

The app is built around an explicit **file list**, not a path text box:

- **Add PST files…** — native Windows file picker (`rfd`, multi-select) filtered to `.pst`/`.ost`.
- **Add folder…** — native folder picker; the folder is scanned recursively for `.pst`/`.ost`.
- **Drag & drop** `.pst` files or folders straight onto the window.
- **Files to load** list shows each source with a status tag (queued → indexed); the `x` button
  removes a source from the list without touching the index.
- **Index selected** indexes every queued source. Indexing runs on a **worker thread** (its own
  SQLite connection) so the UI never blocks; per-file results stream into the ingest log.
- **Loaded in the index** lists every PST currently in the index with its item count, and each row
  has an **unload** button that removes exactly the items that came only from that PST (their FTS
  rows too), leaving other PSTs' copies intact.

## Highlighting search hits

Matched terms are highlighted in the results table (subject) and in the message pane (subject line
and the whole body) with an amber background. Highlighting comes from FTS5 itself —
`highlight(fts, <col>, <open>, <close>)` — not from a client-side substring guess, so what is
marked is exactly what matched (stemming/prefix rules included). The markers are the control
characters `\u{1}`/`\u{2}` (they cannot occur in mail text), parsed into an egui `LayoutJob`.
Clean text is kept separately for export and the copy button.

**Column order matters**: the `fts` table is declared
`fts5(subject, sender, sender_email, rcpt, body, att_names, folder)`, so **body is column 4** and
`att_names` is 5. Passing the wrong index silently returns the wrong column's text — the first
build of this pane rendered attachment filenames as the message body because of exactly that.

## Test corpus (generated locally; `corpus/` is not committed)

| File | Size | Contents |
|---|---|---|
| `mailbox-100mb.pst` | 104.5 MB | 1,660 messages — 1,620 unique + **40 byte-identical duplicates** in `Legal Holds/Duplicates`, 218 with attachments |
| `mailbox-20mb.pst` | 20.7 MB | 339 messages, same shape |
| `v1-flat.pst` | 5.8 MB | 300 messages, root/Inbox only (used to isolate a writer issue) |

Shape: folders `Inbox`/`Sent Items`/`Deleted Items`/`Projects/{2021 Northgate, 2022 Settlement,
2023 Audit}`/`Legal Holds/Duplicates`, 1–4 To/Cc/Bcc recipients each, dates 2018-01→2025-12,
Zipf-skewed vocabulary so search selectivity is realistic (common terms match most of the corpus,
rare ones match a handful), ~5% CJK/accented subjects, attachments named like
`statement_0.pdf`/`invoice_1.xlsx` with real magic bytes.

Regenerate with `pstgen` (uses `outlook-pst-rw`, the MIT write fork):

```bash
cd pstgen && cargo build --release
cargo run --release -- out.pst --messages 1620 --attach-every 12 --attach-kb 240 --dup-every 40
```

**Validity — measured, and it is reader-dependent:**

| Reader | Result on `mailbox-100mb.pst` |
|---|---|
| Microsoft's Rust reader (`outlook-pst`) | ✅ all 1,660 messages, class + recipients + msg_id on every one |
| **Outlook 16** (`AddStore` + full folder walk) | ✅ opens in 0.0 s, **1,660 items**, correct folder tree, CJK subject and recipient intact |
| libpff / pypff / `pff-tools` | ❌ `libpff_table_read_values_array: mismatch in values array identifier` when listing sub-folders |

libpff reads the same writer's empty PST and Outlook-made PSTs fine, and the failure reproduces on
a 300-message root-only file — so the trigger is the write fork's *message-append* path, not folder
nesting or size. It is therefore **not a drop-in stand-in for a Purview export when testing
libpff-based tools**; it is fine for this app (Microsoft's reader) and for Outlook. See
`outlook_validate2.ps1` and `verify_libpff.py` for the two harnesses.

Measured against the app on Windows: 1,660 messages indexed from the 104.5 MB file in **0.418 s**
(~3,970 msgs/s), 40 duplicate occurrences collapsed to 1,620 unique items with provenance kept.

## Layout

```
src/lib.rs           shared engine: PST parsing, SQLite FTS5 index, search. No UI deps.
src/bin/gui.rs       the desktop app (egui/eframe), built as a Windows-subsystem binary
src/bin/pst-cli.rs   the same engine with console output, for scripting and bulk ingest
src/main.rs          minimal parser probe (prints every property of every message)
examples/fts.rs             parse -> SQLite FTS5 -> search, console only
pstgen/                     generates the synthetic PST corpus (writes Unicode PSTs)
tools/python-poc/           earlier Python/pypff proof of concept: indexer + dedupe/1M benchmarks
tools/windows/              PowerShell harnesses used to verify the build on Windows

corpus/  dist/              NOT committed - generated PSTs and built binaries (see Releases)
```

### Test data is not in this repo

The corpus is regenerated with `pstgen` (see above), so nothing large is stored here. The two small
fixture PSTs used during early development (`alpha-beta-gamma-delta.pst`, `contacts.pst`) come from
[bod09/pst-viewer](https://github.com/bod09/pst-viewer) - synthetic files from the MIT-licensed
`pst-extractor` and Apache-2.0 `msgreader` test suites - and are not redistributed here.

### Verifying on Windows (`tools/windows/`)

`win_probe.ps1` probes the machine (runtimes, WebView2, .NET, disks); `close_test.ps1` launches the
app, sends `WM_CLOSE` and asserts the process exits; `fallback_test.ps1` proves the
non-writable-folder fallback; `portable_test.ps1` runs the exe from an otherwise empty folder;
`outlook_validate2.ps1` validates a PST against Outlook itself over COM (add store, walk, remove,
leaving the profile as found).

## Build for Windows (from Linux — no MSVC needed on the target)

```bash
# one-time
rustup target add x86_64-pc-windows-gnu
sudo apt-get install -y gcc-mingw-w64-x86-64
cat > ~/.cargo/config.toml <<'EOF'
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-ar"
EOF

# every build
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
cargo build --release --target x86_64-pc-windows-gnu --bin pst-searcher --bin pst-cli
```

`pst-searcher.exe` is a **Windows-subsystem** binary (`Subsystem: 00000002`), so double-clicking it
opens only the window - no console flash. That also means it cannot print to a console, which is why
the command-line mode lives in a separate **console-subsystem** binary (`pst-cli.exe`,
`Subsystem: 00000003`) built from the same library. Check with
`x86_64-w64-mingw32-objdump -p <exe> | grep Subsystem`.

Native Linux build (for development): `cargo build --release --bin gui`.

## Run

GUI: double-click `pst-searcher.exe`. It creates `pst-index.db` beside itself, or

```
pst-searcher.exe --db C:\path\to\index.db
```

**Where the index goes.** With `--db` that path is used, and if it cannot be opened you get an error
dialog rather than a silent exit. Without `--db` the program's own folder is tried first and then
`%LOCALAPPDATA%\PstSearcher`, so the app still starts when it sits somewhere non-writable such as
`C:\Program Files`. When it falls back, the path is shown in the header and noted in the ingest log.

CLI (bulk ingest / scripting / CI — identical index and search code):

```
pst-cli.exe --db C:\cases\index.db --index C:\cases\export      # ingest a file, or every PST under a folder
pst-cli.exe --db C:\cases\index.db --search "invoice AND contract"
pst-cli.exe --counts                                              # index stats
pst-cli.exe --help
```

The GUI also accepts `--query "<fts>"`, `--paths "<a.pst;folder>"`, `--auto-index` and
`--select-first` (used to drive it for screenshots and automated checks).

## Search syntax

Lucene-ish FTS5: `invoice AND contract`, `"wire transfer"`, `audit NOT review`,
`subject:termination`, `sender_email:"user42@example.com"`, `contr*`.

Raw user input is escaped before it reaches FTS5 — `sender_email:a@b.com` is a
syntax error in FTS5 and must become `sender_email:"a@b.com"`; `escape_fts_query()`
does this and the GUI shows the escaped query it actually ran. Dates, sender and
folder are filter columns, not FTS terms: searching `2020-01-01` matches nothing.

## Known gaps (measured, not guessed)

- **Attachments are listed, not extracted.** Names and counts are indexed; the bytes
  are not yet exported or text-extracted (no PDF/Office/OCR into the index).
- **`PR_ATTACH_SIZE` (0x0E20) in the attachment table is not the file length.** It
  reported 3869 bytes for a file that is actually 237 bytes (confirmed against
  `pffexport` output). Take sizes from the attachment object, not the table column.
- **Recipient quality is unverified on real data.** These fixtures have no recipient
  table, so the recipient path has never returned a row. Validate on a real Purview
  export before relying on "mail to X" searches.
- **Dedupe is currently global.** One item, many occurrences — right for "found in N
  mailboxes", but confirm it matches your review policy.
- **A full CJK font inflates memory** (~190 MB working set). Subsetting the font, or
  bundling a smaller one, is the obvious fix.
- Index sizing from the 1M-item benchmark: ~3.5 KB per item, ~4,500 items/s write.

## Provenance

Reads PSTs directly per the MS-PST open specification. No Office, Outlook, Exchange or
Graph dependency; PST files are treated as inert documents. Writing PSTs is possible in
principle (`outlook-pst-rw`, an MIT fork adding creation/append) but it is new and
low-adoption, so exports are `.eml` for now.