//! PST Searcher - desktop app: pick PST files, index them, search, export messages.
//!
//! Built as a Windows-subsystem binary so double-clicking it opens only the window,
//! with no console flash. The console-mode equivalent is `pst-cli.exe`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use eframe::egui;
use rusqlite::Connection;

use pstsearch::*;

fn marked_job(text: &str, font: egui::FontId, wrap_width: f32, base: egui::Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let mut cur = String::new();
    let mut hot = false;
    let mut flush = |job: &mut egui::text::LayoutJob, cur: &mut String, hot: bool| {
        if cur.is_empty() {
            return;
        }
        let fmt = if hot {
            egui::TextFormat {
                font_id: font.clone(),
                color: egui::Color32::from_rgb(20, 20, 20),
                background: egui::Color32::from_rgb(255, 214, 102),
                ..Default::default()
            }
        } else {
            egui::TextFormat { font_id: font.clone(), color: base, ..Default::default() }
        };
        job.append(cur, 0.0, fmt);
        cur.clear();
    };
    for ch in text.chars() {
        if ch == HL_OPEN_CH {
            flush(&mut job, &mut cur, hot);
            hot = true;
        } else if ch == HL_CLOSE_CH {
            flush(&mut job, &mut cur, hot);
            hot = false;
        } else {
            cur.push(ch);
        }
    }
    flush(&mut job, &mut cur, hot);
    job
}

enum Msg {
    FileStart(String),
    FileDone { name: String, msgs: usize, new: usize, dup: usize, err: Option<String> },
    Finished,
}

#[derive(Clone)]
enum SrcState {
    Queued,
    Working,
    Complete,
    Failed(String),
}

struct Source {
    path: PathBuf,
    state: SrcState,
}

struct App {
    conn: Connection,
    db_path: PathBuf,
    db_label: String,
    sources: Vec<Source>,
    rx: Option<mpsc::Receiver<Msg>>,
    busy: bool,
    log: Vec<String>,
    query: String,
    hits: Vec<Hit>,
    selected: Option<usize>,
    detail_head: String,
    detail_subject: String,
    detail_body: String,
    status: String,
    export_dir: String,
    settings: Settings,
    /// Guard so the cleanup runs at most once per process.
    cleaned_up: bool,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        conn: Connection,
        db_path: PathBuf,
        note: Option<String>,
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        let export_dir = export_dir_for(&db_path);
        let argv: Vec<String> = std::env::args().collect();

        // --clean-on-exit / --clean-exports force the cleanup for this run only, without
        // changing the saved choice (scripted or one-off use).
        let mut settings = Settings::load(&db_path);
        if argv.iter().any(|a| a == "--clean-on-exit") {
            settings.clean_on_exit = true;
        }
        if argv.iter().any(|a| a == "--clean-exports") {
            settings.clean_exports = true;
            settings.clean_on_exit = true;
        }

        let mut app = Self {
            conn,
            db_label: db_path.display().to_string(),
            db_path,
            sources: Vec::new(),
            rx: None,
            busy: false,
            log: Vec::new(),
            query: String::new(),
            hits: Vec::new(),
            selected: None,
            detail_head: String::new(),
            detail_subject: String::new(),
            detail_body: String::new(),
            status: "Add PST files or a folder, then Index.".into(),
            export_dir: export_dir.display().to_string(),
            settings,
            cleaned_up: false,
        };

        // --paths <a.pst;b.pst|dir> preloads the source list (scripting / demos)
        if let Some(i) = argv.iter().position(|a| a == "--paths") {
            if let Some(list) = argv.get(i + 1) {
                let paths: Vec<PathBuf> = list
                    .split(';')
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from)
                    .collect();
                app.add_paths(paths);
            }
        }
        if let Some(i) = argv.iter().position(|a| a == "--query") {
            if let Some(q) = argv.get(i + 1) {
                app.query = q.clone();
                app.do_search();
            }
        }
        // --auto-index begins indexing as soon as the window opens (demos / scripting).
        // NB: --index is the *headless* flag; keep them distinct.
        if let Some(n) = note {
            app.log.push(format!("note: {n}"));
            app.status = format!("Ready. Note: {n}");
        }
        if argv.iter().any(|a| a == "--select-first") && !app.hits.is_empty() {
            app.selected = Some(0);
            let id = app.hits[0].id;
            app.load_detail(id);
        }
        if argv.iter().any(|a| a == "--auto-index") {
            app.start_index();
        }
        app
    }

    fn loaded_psts(&self) -> Vec<(String, i64)> {
        let mut stmt = match self.conn.prepare(
            "SELECT i.pst, COUNT(*) FROM item i GROUP BY i.pst ORDER BY 2 DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        let mut added = 0;
        for p in paths {
            if self.sources.iter().any(|s| s.path == p) {
                continue;
            }
            if !p.exists() {
                self.log.push(format!("skipped (not found): {}", p.display()));
                continue;
            }
            let is_pst = p.is_file();
            let n = if is_pst { 1 } else { pst_files(&p).len() };
            if n == 0 {
                self.log.push(format!("no .pst/.ost found in {}", p.display()));
                continue;
            }
            self.sources.push(Source { path: p.clone(), state: SrcState::Queued });
            self.log.push(format!(
                "added {}: {} PST file{}",
                short_name(&p),
                n,
                if n == 1 { "" } else { "s" }
            ));
            added += 1;
        }
        if added > 0 {
            let total: usize = self.sources.iter().map(|s| if s.path.is_file() { 1 } else { pst_files(&s.path).len() }).sum();
            self.status = format!("{} source(s) selected, {total} PST file(s) ready to index.", self.sources.len());
        }
    }

    fn start_index(&mut self) {
        if self.busy {
            return;
        }
        let ready: Vec<PathBuf> = self
            .sources
            .iter()
            .filter(|s| !matches!(s.state, SrcState::Complete))
            .map(|s| s.path.clone())
            .collect();
        if ready.is_empty() {
            self.status = "Add a PST file or a folder first.".into();
            return;
        }
        for s in self.sources.iter_mut() {
            s.state = SrcState::Queued;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.busy = true;
        self.status = "Indexing…".into();

        let db = self.db_path.clone();
        std::thread::spawn(move || {
            // own connection in the worker so the UI thread never blocks on the write
            let conn = Connection::open(&db);
            let mut conn = match conn {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Msg::FileDone { name: "(index db)".into(), msgs: 0, new: 0, dup: 0, err: Some(e.to_string()) });
                    let _ = tx.send(Msg::Finished);
                    return;
                }
            };
            if let Err(e) = init_db(&conn) {
                let _ = tx.send(Msg::FileDone { name: "(schema)".into(), msgs: 0, new: 0, dup: 0, err: Some(e.to_string()) });
                let _ = tx.send(Msg::Finished);
                return;
            }
            for path in ready {
                let files = pst_files(&path);
                if files.is_empty() {
                    let _ = tx.send(Msg::FileDone {
                        name: short_name(&path),
                        msgs: 0,
                        new: 0,
                        dup: 0,
                        err: Some("no .pst/.ost files found".into()),
                    });
                    continue;
                }
                for f in files {
                    let name = short_name(&f);
                    let _ = tx.send(Msg::FileStart(name.clone()));
                    match index_one(&mut conn, &f) {
                        Ok((m, n, d)) => {
                            let _ = tx.send(Msg::FileDone { name, msgs: m, new: n, dup: d, err: None });
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::FileDone { name, msgs: 0, new: 0, dup: 0, err: Some(e) });
                        }
                    }
                }
            }
            let _ = tx.send(Msg::Finished);
        });
    }

    fn poll_worker(&mut self) {
        let Some(rx) = self.rx.take() else { return };
        let mut finished = false;
        let mut current: Option<String> = None;
        loop {
            match rx.try_recv() {
                Ok(Msg::FileStart(name)) => {
                    current = Some(name.clone());
                    self.status = format!("Indexing {name}…");
                }
                Ok(Msg::FileDone { name, msgs, new, dup, err }) => {
                    match err {
                        Some(e) => self.log.push(format!("ERROR {name}: {e}")),
                        None => self.log.push(format!(
                            "{name}: {msgs} message(s) → {new} new, {dup} duplicate occurrence(s)"
                        )),
                    }
                }
                Ok(Msg::Finished) => {
                    finished = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        let _ = &current;
        if self.log.len() > 500 {
            let drop_n = self.log.len() - 500;
            self.log.drain(0..drop_n);
        }
        if finished {
            self.busy = false;
            for s in self.sources.iter_mut() {
                if !matches!(s.state, SrcState::Failed(_)) {
                    s.state = SrcState::Complete;
                }
            }
            let (items, occ, psts) = self.counts();
            let summary = format!(
                "Index built: {items} unique item(s), {occ} occurrence(s) across {psts} PST(s)."
            );
            // the visible results can be stale after new files are indexed: refresh them
            if self.query.trim().is_empty() {
                self.status = summary;
            } else {
                self.do_search();
                let hits = self.status.clone();
                self.status = format!("{summary}   {hits}");
            }
        } else {
            self.rx = Some(rx);
        }
    }

    fn counts(&self) -> (i64, i64, i64) {
        db_counts(&self.conn)
    }

    fn save_settings(&mut self) {
        match self.settings.save(&self.db_path) {
            Ok(p) => self.log.push(format!("settings saved to {}", p.display())),
            Err(e) => self.status = format!("could not save settings: {e}"),
        }
    }

    /// Delete the files this app created, on the way out.
    ///
    /// Order is forced: our own connection - and, while indexing is still running, the
    /// worker's - holds the database open, and Windows will not unlink an open file. So
    /// wait briefly for the worker, close our handle, and only then delete. Swapping the
    /// connection for an in-memory one drops the old handle without turning `conn` into
    /// an Option at every one of its use sites.
    fn cleanup_on_exit(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.busy && std::time::Instant::now() < deadline {
            self.poll_worker();
            if self.busy {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }

        if let Ok(fresh) = Connection::open_in_memory() {
            let old = std::mem::replace(&mut self.conn, fresh);
            drop(old);
        }
        let report = clean_all(&self.db_path, self.settings.clean_exports);
        // The window is on its way out, so there is nowhere to show this - but keep it on
        // stderr for anyone who redirected it (`pst-searcher.exe 2>cleanup.log`).
        eprintln!("cleanup on exit: {}", report.describe());
    }

    fn do_search(&mut self) {
        match search(&self.conn, &self.query) {
            Ok(hits) => {
                self.hits = hits;
                self.selected = None;
                self.detail_head.clear();
                self.detail_subject.clear();
                self.detail_body.clear();
                self.status = format!(
                    "{} hit(s) for  {}  →  {}",
                    self.hits.len(),
                    self.query,
                    escape_fts_query(&self.query)
                );
            }
            Err(e) => self.status = format!("Query failed: {e}"),
        }
    }

    fn load_detail(&mut self, id: i64) {
        let row: Option<(String, String, String, String, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT subject, sender, date, class, folder, body, att_count FROM item WHERE item_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .ok();
        let mut occ: Vec<String> = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT pst, folder FROM occurrence WHERE item_id=?1") {
            if let Ok(rows) = stmt.query_map([id], |r| {
                Ok(format!("{}  ::  {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) {
                occ = rows.filter_map(|r| r.ok()).collect();
            }
        }
        match row {
            Some((subject, sender, date, class, folder, body, att)) => {
                // exact highlighting from FTS5 when this item is part of the current match
                let (subj_disp, body_disp) = match detail_marked(&self.conn, id, &self.query) {
                    Some((s, b)) => (s, b),
                    None => (subject.clone(), body.clone()),
                };
                self.detail_head = format!(
                    "From    : {sender}\nDate    : {date}\nClass   : {class}\n\
                     Folder  : {folder}\nAttach  : {att}\nFound in {} location(s):\n{}",
                    occ.len(),
                    occ.iter().map(|o| format!("  - {o}")).collect::<Vec<_>>().join("\n")
                );
                self.detail_subject = subj_disp;
                self.detail_body = body_disp;
            }
            None => {
                self.detail_head = "No item loaded.".to_string();
                self.detail_subject.clear();
                self.detail_body.clear();
            }
        }
    }

    /// Copy the selected message as text (markers stripped).
    fn copy_selected(&mut self, ctx: &egui::Context) {
        if self.detail_body.is_empty() && self.detail_subject.is_empty() {
            self.status = "Select a result first.".into();
            return;
        }
        let text = format!(
            "Subject: {}\n\n{}\n\n{}\n",
            strip_marks(&self.detail_subject),
            strip_marks(&self.detail_head),
            strip_marks(&self.detail_body)
        );
        ctx.copy_text(text);
        self.status = "Copied the selected message to the clipboard.".into();
    }

    fn export_selected(&mut self) {
        let Some(idx) = self.selected else {
            self.status = "Select a result first.".into();
            return;
        };
        let hit = &self.hits[idx];
        let id = hit.id;
        let row: Option<(String, String, String, String, String, String)> = self
            .conn
            .query_row(
                "SELECT subject, sender, sender_email, rcpt, date, body FROM item WHERE item_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .ok();
        let Some((subject, sender, sender_email, rcpt, date, body)) = row else {
            self.status = "Item vanished.".into();
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&self.export_dir) {
            self.status = format!("Cannot create export dir: {e}");
            return;
        }
        let safe: String = subject
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .take(60)
            .collect();
        let path = Path::new(&self.export_dir).join(format!("{id:06}_{safe}.eml"));
        let from = if sender_email.is_empty() { sender } else { sender_email };
        let eml = format!(
            "Subject: {subject}\r\nFrom: {from}\r\nTo: {rcpt}\r\nDate: {date}\r\n\
             X-PST-Source: {} :: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n",
            hit.pst, hit.folder
        );
        self.status = match std::fs::write(&path, eml.as_bytes()) {
            Ok(_) => format!("Exported {} bytes -> {}", eml.len(), path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    /// Drop every item that came only from this PST, and its search index rows.
    fn unload_pst(&mut self, pst: &str) {
        let result = (|| -> rusqlite::Result<usize> {
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM occurrence WHERE pst=?1", [pst])?;
            let ids: Vec<i64> = {
                let mut stmt = tx.prepare(
                    "SELECT item_id FROM item WHERE item_id NOT IN (SELECT item_id FROM occurrence)",
                )?;
                let v: Vec<i64> = stmt.query_map([], |r| r.get(0))?.filter_map(|r| r.ok()).collect();
                v
            };
            for id in &ids {
                tx.execute("DELETE FROM fts WHERE rowid=?1", [id])?;
                tx.execute("DELETE FROM item WHERE item_id=?1", [id])?;
            }
            tx.commit()?;
            Ok(ids.len())
        })();
        match result {
            Ok(n) => {
                self.hits.retain(|h| h.pst != pst);
                self.selected = None;
                self.detail_head.clear();
                self.detail_subject.clear();
                self.detail_body.clear();
                let (items, occ, psts) = self.counts();
                self.status = format!(
                    "Unloaded {pst}: removed {n} item(s). Index now {items} item(s) / {occ} occurrence(s) across {psts} PST(s)."
                );
            }
            Err(e) => self.status = format!("Unload failed: {e}"),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Closing the window must actually end the process. Observed on Windows: after the
        // window was closed the process kept running with no top-level window (alive,
        // "responding", ~210 MB) until it was killed. Nothing here needs flushing (SQLite
        // commits per file), so exit explicitly rather than trust the viewport teardown.
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            if self.settings.clean_on_exit {
                self.cleanup_on_exit();
            }
            std::process::exit(0);
        }
        self.poll_worker();
        if self.busy {
            ui.ctx().request_repaint();
        }

        // drag & drop PST files / folders straight onto the window
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_paths(dropped);
        }

        let (items, occ, psts) = self.counts();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("PST Searcher");
                ui.separator();
                ui.label(format!("{items} unique items · {occ} occurrences · {psts} PSTs"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&self.db_label).weak().small());
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::top("controls").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Add PST files…").clicked() {
                    if let Some(files) = rfd::FileDialog::new()
                        .add_filter("Outlook data files", &["pst", "ost"])
                        .add_filter("All files", &["*"])
                        .set_title("Select PST files to load")
                        .pick_files()
                    {
                        self.add_paths(files);
                    }
                }
                if ui.button("Add folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Select a folder to scan for .pst/.ost")
                        .pick_folder()
                    {
                        self.add_paths(vec![dir]);
                    }
                }
                let can_index = !self.sources.is_empty() && !self.busy;
                if ui
                    .add_enabled(can_index, egui::Button::new("Index selected"))
                    .clicked()
                {
                    self.start_index();
                }
                if ui.add_enabled(!self.sources.is_empty(), egui::Button::new("Clear list")).clicked() {
                    self.sources.clear();
                    self.status = "Source list cleared (the index is unchanged).".into();
                }
                if self.busy {
                    ui.spinner();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Search:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .desired_width(520.0)
                        .hint_text("e.g. invoice AND contract  |  subject:termination  |  sender_email:\"a@b.com\""),
                );
                let go = ui.button("Search").clicked()
                    || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    self.do_search();
                }
                if ui.button("Export selected .eml").clicked() {
                    self.export_selected();
                }
            });
            if !self.query.is_empty() {
                ui.label(
                    egui::RichText::new(format!("FTS5 query: {}", escape_fts_query(&self.query)))
                        .weak()
                        .small(),
                );
            }
            ui.horizontal(|ui| {
                let mut s = self.settings;
                let mut changed = false;
                if ui
                    .checkbox(&mut s.clean_on_exit, "Delete the index when this window closes")
                    .changed()
                {
                    if !s.clean_on_exit {
                        // nothing to clean the exports for any more
                        s.clean_exports = false;
                    }
                    changed = true;
                }
                let also_exports = s.clean_on_exit;
                ui.add_enabled_ui(also_exports, |ui| {
                    if ui
                        .checkbox(&mut s.clean_exports, "…and the exported .eml files")
                        .changed()
                    {
                        changed = true;
                    }
                });
                if changed {
                    self.settings = s;
                    self.save_settings();
                }
                if self.settings.clean_on_exit {
                    let what = if self.settings.clean_exports {
                        "the index and its exported .eml files"
                    } else {
                        "the index"
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "on close: remove {what} - your PST files are never touched"
                        ))
                        .weak()
                        .small(),
                    );
                }
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            ui.label(egui::RichText::new(&self.status).small());
            ui.add_space(2.0);
        });

        egui::Panel::left("sources").default_size(430.0).show(ui, |ui| {
            ui.add_space(4.0);
            ui.strong(format!("Files to load ({})", self.sources.len()));
            ui.label(
                egui::RichText::new("Browse, or drag .pst files / folders onto the window.")
                    .weak()
                    .small(),
            );
            ui.separator();
            let mut remove: Option<usize> = None;
            egui::ScrollArea::vertical().max_height(190.0).id_salt("srcs").show(ui, |ui| {
                for (i, s) in self.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let tag = match &s.state {
                            SrcState::Queued => "· queued",
                            SrcState::Working => "· working",
                            SrcState::Complete => "· indexed",
                            SrcState::Failed(e) => {
                                ui.colored_label(egui::Color32::LIGHT_RED, "· failed");
                                let _ = e;
                                ""
                            }
                        };
                        if ui.small_button("x").clicked() {
                            remove = Some(i);
                        }
                        let label = if s.path.is_file() {
                            s.path.display().to_string()
                        } else {
                            format!("{}  (folder)", s.path.display())
                        };
                        ui.label(label);
                        if !tag.is_empty() {
                            ui.label(egui::RichText::new(tag).weak().small());
                        }
                    });
                }
                if self.sources.is_empty() {
                    ui.label(egui::RichText::new("nothing selected yet").weak());
                }
            });
            if let Some(i) = remove {
                self.sources.remove(i);
            }

            ui.add_space(6.0);
            ui.strong("Loaded in the index");
            ui.separator();
            let loaded = self.loaded_psts();
            let mut unload: Option<String> = None;
            egui::ScrollArea::vertical().id_salt("loaded").show(ui, |ui| {
                if loaded.is_empty() {
                    ui.label(egui::RichText::new("no PST indexed yet").weak());
                }
                for (name, n) in &loaded {
                    ui.horizontal(|ui| {
                        if ui.small_button("unload").clicked() {
                            unload = Some(name.clone());
                        }
                        ui.label(format!("{name}  —  {n} item(s)"));
                    });
                }
                if !self.log.is_empty() {
                    ui.add_space(6.0);
                    ui.strong("Ingest log");
                    ui.separator();
                    for line in self.log.iter().rev().take(60) {
                        ui.label(egui::RichText::new(line).small().monospace());
                    }
                }
            });
            if let Some(name) = unload {
                self.unload_pst(&name);
            }
        });

        let mut copy_now = false;
        egui::Panel::right("detail").default_size(520.0).show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.strong("Message");
                if ui.small_button("copy").clicked() {
                    copy_now = true;
                }
                ui.label(egui::RichText::new("matched terms highlighted").weak().small());
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mono = egui::TextStyle::Monospace.resolve(ui.style());
                let base = ui.visuals().text_color();
                let w = ui.available_width();
                if !self.detail_head.is_empty() {
                    ui.add(egui::Label::new(marked_job(&self.detail_head, mono.clone(), w, base)));
                }
                if !self.detail_subject.is_empty() || !self.detail_body.is_empty() {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(marked_job(
                        &format!("Subject : {}", self.detail_subject),
                        mono.clone(),
                        w,
                        base,
                    )));
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add(egui::Label::new(marked_job(&self.detail_body, mono.clone(), w, base)));
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let mut clicked: Option<usize> = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("hits")
                    .striped(true)
                    .num_columns(5)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("Date");
                        ui.strong("Subject");
                        ui.strong("From");
                        ui.strong("Att");
                        ui.strong("Where");
                        ui.end_row();
                        for (i, h) in self.hits.iter().enumerate() {
                            let sel = self.selected == Some(i);
                            if ui.selectable_label(sel, &h.date).clicked() {
                                clicked = Some(i);
                            }
                            let font = egui::TextStyle::Body.resolve(ui.style());
                            let job = marked_job(&h.subject_hl, font, 460.0, ui.visuals().text_color());
                            if ui.selectable_label(sel, job).clicked() {
                                clicked = Some(i);
                            }
                            if ui.selectable_label(sel, &h.sender).clicked() {
                                clicked = Some(i);
                            }
                            if ui.selectable_label(sel, format!("{}", h.att_count)).clicked() {
                                clicked = Some(i);
                            }
                            let where_ = if h.occurrences > 1 {
                                format!("{} ({} copies)", h.pst, h.occurrences)
                            } else {
                                h.pst.clone()
                            };
                            if ui.selectable_label(sel, where_).clicked() {
                                clicked = Some(i);
                            }
                            ui.end_row();
                        }
                    });
            });
            if let Some(i) = clicked {
                self.selected = Some(i);
                let id = self.hits[i].id;
                self.load_detail(id);
            }
            if copy_now {
                let ctx = ui.ctx().clone();
                self.copy_selected(&ctx);
            }
        });
    }
}

fn install_fonts(ctx: &egui::Context) {
    // egui ships Latin fonts only; CJK mail subjects render as tofu without one.
    let candidates = [
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\YuGothM.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
        r"C:\Windows\Fonts\msmincho.ttc",
        r"C:\Windows\Fonts\malgun.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    let mut fonts = egui::FontDefinitions::default();
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let name = "cjk".to_owned();
        fonts
            .font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let explicit = flag("--db").map(PathBuf::from);
    let (conn, db_path, note) = match open_index(explicit) {
        Ok(v) => v,
        Err(e) => {
            // windowed build: there is no console to print to, so use a dialog
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("PST Searcher cannot start")
                .set_description(format!(
                    "The index database could not be opened.\n\n{e}\n\n\
                     Run the app from a folder you can write to, or pass --db <path>."
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            std::process::exit(1);
        }
    };

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1420.0, 860.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("PST Searcher"),
        ..Default::default()
    };
    eframe::run_native(
        "PST Searcher",
        opts,
        Box::new(move |cc| Ok(Box::new(App::new(cc, conn, db_path, note)))),
    )
}
