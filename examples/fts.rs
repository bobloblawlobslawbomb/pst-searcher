//! Prove the full lightweight stack: parse PST -> SQLite FTS5 index -> search.
use outlook_pst::messaging::folder::Folder;
use outlook_pst::messaging::store::Store;
use outlook_pst::ndb::node_id::NodeId;

const PR_SUBJECT: u16 = 0x0037;
const PR_MESSAGE_CLASS: u16 = 0x001A;
const PR_SENDER_NAME: u16 = 0x0C1A;
const PR_BODY: u16 = 0x1000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = rusqlite::Connection::open_in_memory()?;
    println!("sqlite version: {}", rusqlite::version());
    let fts5: Option<String> = conn.query_row(
        "select compile_options from pragma_compile_options where compile_options like '%FTS5%'",
        [], |r| r.get(0)).ok();
    println!("FTS5 compiled in: {}", fts5.unwrap_or_else(|| "NO".into()));
    conn.execute_batch(
        "CREATE TABLE item(id INTEGER PRIMARY KEY, subject TEXT, sender TEXT, class TEXT, body TEXT,
                           source TEXT, folder TEXT);
         CREATE VIRTUAL TABLE fts USING fts5(subject, sender, body, folder, tokenize='unicode61');")?;

    let mut n = 0;
    for path in std::env::args().skip(1) {
        let store = outlook_pst::open_store(std::path::Path::new(&path))?;
        let root = store.open_folder(&store.properties().ipm_sub_tree_entry_id()?)?;
        let mut stack = vec![root];
        while let Some(folder) = stack.pop() {
            let fname = folder.properties().display_name().map(|s| s.to_string()).unwrap_or_default();
            if let Some(contents) = folder.contents_table() {
                for row in contents.rows_matrix() {
                    let Ok(eid) = store.properties().make_entry_id(NodeId::from(u32::from(row.id()))) else { continue };
                    let Ok(msg) = store.open_message(&eid, None) else { continue };
                    let p = msg.properties();
                    let s = |id: u16| p.get(id).map(|v| format!("{v:?}")).unwrap_or_default();
                    let (subj, sender, class, body) =
                        (s(PR_SUBJECT), s(PR_SENDER_NAME), s(PR_MESSAGE_CLASS), s(PR_BODY));
                    conn.execute("INSERT INTO item (subject,sender,class,body,source,folder) VALUES (?,?,?,?,?,?)",
                                 rusqlite::params![subj, sender, class, body, path, fname])?;
                    let id = conn.last_insert_rowid();
                    conn.execute("INSERT INTO fts (rowid,subject,sender,body,folder) VALUES (?,?,?,?,?)",
                                 rusqlite::params![id, subj, sender, body, fname])?;
                    n += 1;
                }
            }
            if let Some(h) = folder.hierarchy_table() {
                for row in h.rows_matrix() {
                    if let Ok(eid) = store.properties().make_entry_id(NodeId::from(u32::from(row.id()))) {
                        if let Ok(sub) = store.open_folder(&eid) { stack.push(sub); }
                    }
                }
            }
        }
    }
    println!("indexed {n} messages");
    for q in ["Alpha", "IPM", "xmailuser", "Alpha OR Contact", "subject:Alpha"] {
        let mut stmt = conn.prepare(
            "SELECT i.subject, bm25(fts) s, snippet(fts, 2, '[', ']', ' ... ', 8)
             FROM fts JOIN item i ON i.id = fts.rowid WHERE fts MATCH ? ORDER BY s LIMIT 3")?;
        let hits: Vec<(String, f64, String)> = stmt.query_map([q], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok()).collect();
        println!("  query {q:?} -> {} top hit(s)", hits.len());
        for (subj, score, frag) in hits.iter().take(2) {
            println!("     score={score:.4} subject={subj} frag={frag}");
        }
    }
    Ok(())
}
