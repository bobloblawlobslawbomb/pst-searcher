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
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "pst-cli [--db <index.db>] [--index <file-or-folder>] [--search <fts query>] [--counts]\n\n\
             --index   index a .pst/.ost file, or every one under a folder\n\
             --search  run an FTS5 query (AND/OR/NOT, field:value, \"phrase\", prefix*)\n\
             --counts  print index size\n\n\
             Without --db the index is taken from the program folder, falling back to\n\
             %LOCALAPPDATA%\\PstSearcher when that folder is not writable."
        );
        return;
    }

    let explicit = flag("--db").map(PathBuf::from);
    let (mut conn, db_path, note) = match open_index(explicit) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: cannot open the index database: {e}");
            std::process::exit(1);
        }
    };
    if let Some(n) = &note {
        println!("note: {n}");
    }

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
