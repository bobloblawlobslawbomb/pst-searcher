//! Shared PST indexing/search core, used by both the GUI and the CLI.
//!
//! Reads PST/OST files directly (no Office, Outlook, mail server or runtime needed),
//! indexes them into SQLite FTS5 and searches them. Keep this crate free of UI
//! dependencies so the same code backs `pst-searcher.exe` and `pst-cli.exe`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use outlook_pst::messaging::folder::Folder;
use outlook_pst::messaging::store::Store;
use outlook_pst::ndb::node_id::NodeId;
use rusqlite::Connection;

pub const PR_MESSAGE_CLASS: u16 = 0x001A;

pub const PR_SUBJECT: u16 = 0x0037;

pub const PR_SENDER_NAME: u16 = 0x0C1A;

pub const PR_SENDER_EMAIL: u16 = 0x0C1F;

pub const PR_DISPLAY_TO: u16 = 0x0E04;

pub const PR_DISPLAY_CC: u16 = 0x0E03;

pub const PR_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;

pub const PR_CLIENT_SUBMIT_TIME: u16 = 0x0039;

pub const PR_INTERNET_MESSAGE_ID: u16 = 0x1035;

pub const PR_BODY: u16 = 0x1000;

pub const PR_ATTACH_LONG_FILENAME: u16 = 0x3704;

pub const PR_ATTACH_FILENAME: u16 = 0x3707;

pub const HL_OPEN: &str = "\u{1}";

pub const HL_CLOSE: &str = "\u{2}";

pub const HL_OPEN_CH: char = '\u{1}';

pub const HL_CLOSE_CH: char = '\u{2}';

pub fn pv_to_string(v: &outlook_pst::ltp::prop_context::PropertyValue) -> String {
    use outlook_pst::ltp::prop_context::PropertyValue as PV;
    match v {
        PV::Unicode(u) => u.to_string(),
        PV::String8(s) => s.to_string(),
        PV::Time(t) => filetime_to_string(*t),
        PV::Integer32(i) => i.to_string(),
        PV::Integer64(i) => i.to_string(),
        PV::Boolean(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

pub fn filetime_to_string(ft: i64) -> String {
    if ft <= 0 {
        return String::new();
    }
    let secs = ft / 10_000_000 - 11_644_473_600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn escape_fts_query(q: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for tok in q.split_whitespace() {
        if tok.eq_ignore_ascii_case("AND") || tok.eq_ignore_ascii_case("OR") || tok.eq_ignore_ascii_case("NOT") {
            out.push(tok.to_uppercase());
            continue;
        }
        if let Some((field, value)) = tok.split_once(':') {
            if !field.is_empty() && field.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let v = value.trim_matches('*');
                if let Some(prefix) = value.strip_suffix('*') {
                    out.push(format!("{field}:\"{prefix}\"*"));
                } else {
                    out.push(format!("{field}:\"{v}\""));
                }
                continue;
            }
        }
        if tok.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '*') {
            out.push(tok.to_string());
        } else {
            out.push(format!("\"{}\"", tok.replace('"', "\"\"")));
        }
    }
    if out.is_empty() { "\"\"".to_string() } else { out.join(" ") }
}

pub fn strip_marks(text: &str) -> String {
    text.chars().filter(|c| *c != HL_OPEN_CH && *c != HL_CLOSE_CH).collect()
}

pub fn fingerprint(parts: &[&str]) -> String {
    let mut h = DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
        0u8.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

pub fn short_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

pub struct Extracted {
    pub pst: String,
    pub folder: String,
    pub subject: String,
    pub sender: String,
    pub sender_email: String,
    pub to: String,
    pub cc: String,
    pub date: String,
    pub class: String,
    pub msg_id: String,
    pub body: String,
    pub att_names: String,
    pub att_count: i64,
    pub fingerprint: String,
}

#[derive(Default)]
pub struct Stats {
    pub psts: usize,
    pub messages: usize,
    pub new: usize,
    pub dup: usize,
    pub errors: Vec<String>,
}

pub fn walk_folder(store: &Rc<dyn Store>, folder: &Rc<dyn Folder>, path: &str, pst: &str, out: &mut Vec<Extracted>) {
    let name = folder
        .properties()
        .display_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "(unnamed)".into());
    let full = format!("{path}/{name}");

    if let Some(contents) = folder.contents_table() {
        for row in contents.rows_matrix() {
            let Ok(entry_id) = store.properties().make_entry_id(NodeId::from(u32::from(row.id()))) else {
                continue;
            };
            let Ok(message) = store.open_message(&entry_id, None) else {
                continue;
            };
            let props = message.properties();
            let get = |id: u16| props.get(id).map(pv_to_string).unwrap_or_default();

            let mut att_names: Vec<String> = Vec::new();
            if let Some(attachments) = message.attachment_table() {
                for arow in attachments.rows_matrix() {
                    let ctx = attachments.context();
                    if let Ok(cols) = arow.columns(ctx) {
                        for (column, value) in ctx.columns().iter().zip(cols) {
                            if matches!(column.prop_id(), PR_ATTACH_LONG_FILENAME | PR_ATTACH_FILENAME) {
                                if let Some(v) = value {
                                    if let Ok(v) = attachments.read_column(&v, column.prop_type()) {
                                        att_names.push(pv_to_string(&v));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let subject = get(PR_SUBJECT)
                .trim_start_matches(|c: char| c == '\u{1}' || c == '\u{2}')
                .to_string();
            let date = {
                let d = get(PR_MESSAGE_DELIVERY_TIME);
                if d.is_empty() { get(PR_CLIENT_SUBMIT_TIME) } else { d }
            };
            let body = get(PR_BODY);
            let msg_id = get(PR_INTERNET_MESSAGE_ID);
            let class = get(PR_MESSAGE_CLASS);

            out.push(Extracted {
                pst: pst.to_string(),
                folder: full.clone(),
                fingerprint: fingerprint(&[&msg_id, &subject, &date, &body, &att_names.join(" "), &class]),
                subject,
                sender: get(PR_SENDER_NAME),
                sender_email: get(PR_SENDER_EMAIL),
                to: get(PR_DISPLAY_TO),
                cc: get(PR_DISPLAY_CC),
                date,
                class,
                msg_id,
                body,
                att_count: att_names.len() as i64,
                att_names: att_names.join(" "),
            });
        }
    }

    if let Some(hierarchy) = folder.hierarchy_table() {
        for row in hierarchy.rows_matrix() {
            if let Ok(entry_id) = store.properties().make_entry_id(NodeId::from(u32::from(row.id()))) {
                if let Ok(sub) = store.open_folder(&entry_id) {
                    walk_folder(store, &sub, &full, pst, out);
                }
            }
        }
    }
}

pub fn pst_files(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    fn rec(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    rec(&p, out);
                } else if p
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("pst") || e.eq_ignore_ascii_case("ost"))
                    .unwrap_or(false)
                {
                    out.push(p);
                }
            }
        }
    }
    rec(path, &mut out);
    out.sort();
    out
}

pub fn index_one(conn: &mut Connection, pst_path: &Path) -> Result<(usize, usize, usize), String> {
    let name = short_name(pst_path);
    let store = outlook_pst::open_store(pst_path).map_err(|e| e.to_string())?;
    let root = store
        .properties()
        .ipm_sub_tree_entry_id()
        .and_then(|e| store.open_folder(&e))
        .map_err(|e| e.to_string())?;
    let mut items = Vec::new();
    walk_folder(&store, &root, "", &name, &mut items);
    let seen = items.len();
    let (new, dup) = store_rows(conn, &items).map_err(|e| e.to_string())?;
    Ok((seen, new, dup))
}

pub fn index_path(conn: &mut Connection, path: &Path, stats: &mut Stats) {
    for pst in pst_files(path) {
        stats.psts += 1;
        match index_one(conn, &pst) {
            Ok((m, n, d)) => {
                stats.messages += m;
                stats.new += n;
                stats.dup += d;
            }
            Err(e) => stats.errors.push(format!("{}: {e}", short_name(&pst))),
        }
    }
}

pub fn store_rows(conn: &mut Connection, items: &[Extracted]) -> rusqlite::Result<(usize, usize)> {
    let tx = conn.transaction()?;
    let (mut new, mut dup) = (0usize, 0usize);
    for it in items {
        let existing: Option<i64> = tx
            .query_row("SELECT item_id FROM item WHERE fingerprint=?1", [&it.fingerprint], |r| r.get(0))
            .ok();
        let id = match existing {
            Some(id) => {
                dup += 1;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO item (fingerprint,subject,sender,sender_email,rcpt,date,class,body,pst,folder,att_count,msg_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    rusqlite::params![it.fingerprint, it.subject, it.sender, it.sender_email,
                        format!("{} {}", it.to, it.cc), it.date, it.class, it.body, it.pst, it.folder,
                        it.att_count, it.msg_id],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO fts (rowid,subject,sender,sender_email,rcpt,body,att_names,folder)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![id, it.subject, it.sender, it.sender_email,
                        format!("{} {}", it.to, it.cc), it.body, it.att_names, it.folder],
                )?;
                new += 1;
                id
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO occurrence (item_id,pst,folder) VALUES (?1,?2,?3)",
            rusqlite::params![id, it.pst, it.folder],
        )?;
    }
    tx.commit()?;
    Ok((new, dup))
}

pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS item (
            item_id INTEGER PRIMARY KEY, fingerprint TEXT UNIQUE, subject TEXT, sender TEXT,
            sender_email TEXT, rcpt TEXT, date TEXT, class TEXT, body TEXT, pst TEXT,
            folder TEXT, att_count INTEGER, msg_id TEXT);
         CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
            subject, sender, sender_email, rcpt, body, att_names, folder, tokenize='unicode61 remove_diacritics 2');
         CREATE TABLE IF NOT EXISTS occurrence (
            item_id INTEGER, pst TEXT, folder TEXT, PRIMARY KEY (item_id, pst, folder));
         CREATE INDEX IF NOT EXISTS occ_item ON occurrence(item_id);",
    )
}

pub struct Hit {
    pub id: i64,
    pub subject: String,
    pub subject_hl: String,
    pub sender: String,
    pub date: String,
    pub class: String,
    pub folder: String,
    pub pst: String,
    pub att_count: i64,
    pub frag: String,
    pub occurrences: i64,
}

pub fn search(conn: &Connection, user_query: &str) -> rusqlite::Result<Vec<Hit>> {
    let escaped = escape_fts_query(user_query);
    if escaped == "\"\"" {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT i.item_id, i.subject, i.sender, i.date, i.class, i.folder, i.pst, i.att_count,
                bm25(fts) AS score,
                highlight(fts, 0, ?2, ?3) AS subj_hl,
                snippet(fts, 4, ?2, ?3, ' … ', 14) AS frag,   -- 4 = body (5 is att_names)
                (SELECT COUNT(*) FROM occurrence o WHERE o.item_id = i.item_id) AS occ
         FROM fts JOIN item i ON i.item_id = fts.rowid
         WHERE fts MATCH ?1 ORDER BY score LIMIT 500",
    )?;
    let rows = stmt.query_map(rusqlite::params![&escaped, HL_OPEN, HL_CLOSE], |r| {
        Ok(Hit {
            id: r.get(0)?,
            subject: r.get(1)?,
            subject_hl: r.get(9)?,
            sender: r.get(2)?,
            date: r.get(3)?,
            class: r.get(4)?,
            folder: r.get(5)?,
            pst: r.get(6)?,
            att_count: r.get(7)?,
            frag: r.get(10)?,
            occurrences: r.get(11)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn detail_marked(conn: &Connection, id: i64, user_query: &str) -> Option<(String, String)> {
    let escaped = escape_fts_query(user_query);
    if escaped == "\"\"" {
        return None;
    }
    conn.query_row(
        "SELECT highlight(fts, 0, ?2, ?3), highlight(fts, 4, ?2, ?3)
         FROM fts WHERE fts MATCH ?1 AND rowid = ?4",
        rusqlite::params![&escaped, HL_OPEN, HL_CLOSE, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

pub fn db_counts(conn: &Connection) -> (i64, i64, i64) {
    let one = |sql: &str| conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0);
    (
        one("SELECT COUNT(*) FROM item"),
        one("SELECT COUNT(*) FROM occurrence"),
        one("SELECT COUNT(DISTINCT pst) FROM occurrence"),
    )
}

/// Where the index should live when the caller did not say.
/// Windows: %LOCALAPPDATA%\PstSearcher. Elsewhere: XDG data dir.
pub fn data_dir() -> Option<PathBuf> {
    for var in ["LOCALAPPDATA", "XDG_DATA_HOME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                let leaf = if var == "LOCALAPPDATA" { "PstSearcher" } else { "pst-searcher" };
                return Some(PathBuf::from(v).join(leaf));
            }
        }
    }
    if let Ok(v) = std::env::var("HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v).join(".local/share/pst-searcher"));
        }
    }
    None
}

/// Open (creating if needed) the index database.
///
/// With an explicit path that is the only candidate: if it cannot be opened the caller
/// gets an error and decides how to tell the user. Without one, the program's own folder
/// is tried first and then the per-user data directory, so the app still starts when it
/// sits somewhere non-writable such as C:\Program Files. Returns the connection, the
/// path actually used, and a note when a fallback was taken.
pub fn open_index(explicit: Option<PathBuf>) -> Result<(Connection, PathBuf, Option<String>), String> {
    let mut candidates: Vec<(PathBuf, Option<String>)> = Vec::new();
    match explicit {
        Some(p) => candidates.push((p, None)),
        None => {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|q| q.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."));
            candidates.push((exe_dir.join("pst-index.db"), None));
            if let Some(dir) = data_dir() {
                candidates.push((
                    dir.join("pst-index.db"),
                    Some(format!(
                        "the program folder is not writable, so the index lives in {}",
                        dir.display()
                    )),
                ));
            }
        }
    }
    let mut errors: Vec<String> = Vec::new();
    for (path, note) in candidates {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match Connection::open(&path) {
            Ok(conn) => match init_db(&conn) {
                Ok(()) => return Ok((conn, path, note)),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            },
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    Err(errors.join("  |  "))
}

// ---------------------------------------------------------------------------
// Cleaning up after ourselves
//
// The index holds every message body, so a review machine may not want it still
// sitting there after the app closes. Everything below touches *only* files this
// app created: the index database (three exact paths derived from it) and .eml
// files inside its own export folder. Source PST/OST files are never candidates,
// and neither is anything else that happens to share a folder with them.
// ---------------------------------------------------------------------------

/// Options that persist between runs, stored as `settings.json` beside the index.
/// Hand-rolled rather than pulling in serde: two booleans is not worth a dependency
/// in a binary whose whole point is being small.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Settings {
    /// Delete the index when the app closes.
    pub clean_on_exit: bool,
    /// Also delete the .eml files the app exported.
    pub clean_exports: bool,
}

impl Settings {
    pub fn path_for(db_path: &Path) -> PathBuf {
        db_path.parent().unwrap_or_else(|| Path::new(".")).join("settings.json")
    }

    /// A missing, unreadable or malformed file yields the defaults (both off).
    pub fn load(db_path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path_for(db_path)) else {
            return Self::default();
        };
        Self {
            clean_on_exit: json_flag(&text, "clean_on_exit"),
            clean_exports: json_flag(&text, "clean_exports"),
        }
    }

    pub fn save(&self, db_path: &Path) -> std::io::Result<PathBuf> {
        let p = Self::path_for(db_path);
        std::fs::write(
            &p,
            format!(
                "{{\n  \"clean_on_exit\": {},\n  \"clean_exports\": {}\n}}\n",
                self.clean_on_exit, self.clean_exports
            ),
        )?;
        Ok(p)
    }
}

/// True when `"key"` is present and its value begins with `true`.
fn json_flag(text: &str, key: &str) -> bool {
    let needle = format!("\"{key}\"");
    text.find(&needle)
        .and_then(|i| text[i + needle.len()..].trim_start().strip_prefix(':'))
        .map(|rest| rest.trim_start().starts_with("true"))
        .unwrap_or(false)
}

/// The folder exports are written into (sibling of the index database).
pub fn export_dir_for(db_path: &Path) -> PathBuf {
    db_path.parent().unwrap_or_else(|| Path::new(".")).join("exported")
}

#[derive(Default, Debug)]
pub struct CleanReport {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    /// Files deliberately left alone, or that could not be removed.
    pub skipped: Vec<String>,
}

impl CleanReport {
    pub fn merge(&mut self, other: CleanReport) {
        self.removed.extend(other.removed);
        self.bytes += other.bytes;
        self.skipped.extend(other.skipped);
    }

    pub fn describe(&self) -> String {
        let mut s = if self.removed.is_empty() {
            "nothing to remove".to_string()
        } else {
            format!(
                "removed {} file(s), {:.1} KB",
                self.removed.len(),
                self.bytes as f64 / 1024.0
            )
        };
        for p in &self.removed {
            s.push_str(&format!("\n  - {}", p.display()));
        }
        for k in &self.skipped {
            s.push_str(&format!("\n  ! {k}"));
        }
        s
    }
}

/// Delete the index database and its SQLite sidecars. Only these exact paths are
/// ever unlinked, so a shared or user-chosen folder is safe.
pub fn clean_index(db_path: &Path) -> CleanReport {
    let mut rep = CleanReport::default();
    let mut targets = vec![db_path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(suffix);
        targets.push(PathBuf::from(name));
    }
    for t in targets {
        remove_retrying(&t, &mut rep);
    }
    rep
}

/// Delete only the `.eml` files this app wrote into its export folder. Anything
/// else in there (or in a subfolder) is left alone and reported.
pub fn clean_exports(export_dir: &Path) -> CleanReport {
    let mut rep = CleanReport::default();
    let Ok(entries) = std::fs::read_dir(export_dir) else {
        return rep; // nothing exported yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_eml = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("eml"))
            .unwrap_or(false);
        if !is_eml || !path.is_file() {
            rep.skipped.push(format!("kept (not an export): {}", path.display()));
            continue;
        }
        remove_retrying(&path, &mut rep);
    }
    // tidy the folder away too, but only if it ended up empty
    let _ = std::fs::remove_dir(export_dir);
    rep
}

/// Everything the app created, per the caller's choice.
pub fn clean_all(db_path: &Path, include_exports: bool) -> CleanReport {
    let mut rep = clean_index(db_path);
    if include_exports {
        rep.merge(clean_exports(&export_dir_for(db_path)));
    }
    rep
}

/// Unlink a file, retrying briefly: on Windows another handle (a worker connection,
/// a virus scanner) can hold it for a moment after it is written.
fn remove_retrying(path: &Path, rep: &mut CleanReport) {
    if !path.exists() {
        return;
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    for attempt in 0..10 {
        match std::fs::remove_file(path) {
            Ok(()) => {
                rep.bytes += size;
                rep.removed.push(path.to_path_buf());
                return;
            }
            Err(e) if attempt == 9 => {
                rep.skipped.push(format!("could not delete {}: {e}", path.display()));
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(150)),
        }
    }
}
