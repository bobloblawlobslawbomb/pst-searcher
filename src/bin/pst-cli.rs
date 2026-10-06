//! pst-cli - the same index/search engine as the app, for scripting and bulk ingest.
//! Console subsystem so output is visible when run from a terminal.
use std::path::{Path, PathBuf};

use pstsearch::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let has = |name: &str| args.iter().any(|a| a == name);
    if has("--help") || has("-h") {
        println!(
            "pst-cli [--db <index.db>] [--index <file-or-folder>] [--search <fts query>] [--counts]\n\
             pst-cli [--db <index.db>] [--clean] [--clean-index] [--clean-exports]\n\n\
             --index           index a .pst/.ost file, or every one under a folder\n\
             --search          run an FTS5 query (AND/OR/NOT, field:value, \"phrase\", prefix*)\n\
             --counts          print index size\n\
             --clean           delete the index and the exported .eml files\n\
             --clean-index     delete only the index (with its -wal/-shm sidecars)\n\
             --clean-exports   delete only the exported .eml files\n\n\
             Cleaning never touches the source PST/OST files. Without --db the index is\n\
             taken from the program folder, falling back to %LOCALAPPDATA%\\PstSearcher\n\
             when that folder is not writable."
        );
        return;
    }

    let explicit = flag("--db").map(PathBuf::from);
    let (conn, db_path, note) = match open_index(explicit) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: cannot open the index database: {e}");
            std::process::exit(1);
        }
    };
    if let Some(n) = &note {
        println!("note: {n}");
    }

    // Cleaning is terminal: report it and stop. The connection has to be closed first or
    // Windows refuses to unlink the file it holds open.
    if has("--clean") || has("--clean-index") || has("--clean-exports") {
        let want_index = has("--clean") || has("--clean-index");
        let want_exports = has("--clean") || has("--clean-exports");
        drop(conn);
        let mut report = CleanReport::default();
        if want_index {
            report.merge(clean_index(&db_path));
        }
        if want_exports {
            report.merge(clean_exports(&export_dir_for(&db_path)));
        }
        println!("{}", report.describe());
        return;
    }

    let mut conn = conn;
    if let Some(p) = flag("--index") {
        let mut stats = Stats::default();
        index_path(&mut conn, Path::new(&p), &mut stats);
        println!(
            "indexed {} PST(s): {} message(s) -> {} new, {} duplicate occurrence(s) collapsed",
            stats.psts, stats.messages, stats.new, stats.dup
        );
        for e in &stats.errors {
            println!("  error: {e}");
        }
    }

    let (items, occ, psts) = db_counts(&conn);
    println!(
        "index {} :: {} unique items, {} occurrences, {} PSTs",
        db_path.display(),
        items,
        occ,
        psts
    );

    if let Some(q) = flag("--search") {
        println!("query {:?}  ->  FTS5: {}", q, escape_fts_query(&q));
        match search(&conn, &q) {
            Ok(hits) => {
                println!("{} hit(s)", hits.len());
                for h in hits.iter().take(25) {
                    println!(
                        "  {} | class={} | att={} | {} ({} copies)\n      subj: {}\n      from: {}\n      frag: {}",
                        h.date, h.class, h.att_count, h.pst, h.occurrences,
                        strip_marks(&h.subject_hl), h.sender, strip_marks(&h.frag)
                    );
                }
            }
            Err(e) => println!("  query error: {e}"),
        }
    }
}
