"""Multi-PST benchmark: serial vs parallel ingest, pypff thread safety, cross-PST dedupe."""
import os, sys, glob, time, sqlite3, hashlib, json, multiprocessing as mp
import pypff

HERE = "/home/hermes/.hermes/cache/scratch/pstprobe"
CORPUS = os.path.join(HERE, "corpus")
sys.path.insert(0, HERE)

def extract_one(path):
    """Worker: parse a PST, return item dicts. Pure read, no DB, no shared state."""
    pst = pypff.file()
    pst.open(path)
    items = []
    def walk(folder, path_acc):
        nm = folder.get_name() or "(root)"
        full = f"{path_acc}/{nm}".strip("/")
        for i in range(folder.number_of_sub_messages):
            m = folder.get_sub_message(i)
            body = m.get_plain_text_body()
            body = body.decode("utf-8", "replace") if isinstance(body, bytes) else (body or "")
            atts = []
            for j in range(m.get_number_of_attachments()):
                a = m.get_attachment(j)
                atts.append({"name": a.get_long_filename(), "size": a.get_size(),
                             "sha256": hashlib.sha256(a.read_buffer(a.get_size()) or b"").hexdigest()})
            hdrs = m.get_transport_headers()
            items.append({
                "pst": os.path.basename(path), "folder_path": full,
                "identifier": str(m.get_identifier()), "subject": m.get_subject() or "",
                "sender": m.get_sender_name() or "",
                "date": str(m.get_delivery_time() or m.get_client_submit_time() or ""),
                "body": body,
                "attachments": json.dumps(atts, ensure_ascii=False),
                "att_count": m.get_number_of_attachments(),
                "att_sha": " ".join(a["sha256"] for a in atts),
                "msg_class": "", "raw_headers": hdrs.decode("utf-8", "replace") if isinstance(hdrs, bytes) else (hdrs or ""),
            })
        for i in range(folder.number_of_sub_folders):
            walk(folder.get_sub_folder(i), full)
    walk(pst.get_root_folder(), "")
    pst.close()
    return items

def item_fingerprint(it):
    """Cross-PST identity: Purview duplicates the same message across custodians/searches.
    PR id is PST-local, so hash the content: internet-msg-id if present, else headers+date+subject+body+attachment hashes."""
    import re
    mid = ""
    m = re.search(r"^(?:Message-I[dD]|X-Message-ID):\s*(\S+)", it["raw_headers"] or "", re.M | re.I)
    if m:
        mid = m.group(1).strip().strip("<>")
    basis = "\x1f".join([mid, it["subject"], it["date"], it["body"], it["att_sha"], it["msg_class"]])
    return hashlib.sha256(basis.encode("utf-8", "replace")).hexdigest()

def _worker(path):
    try:
        return os.path.basename(path), extract_one(path), None
    except Exception as e:
        return os.path.basename(path), [], f"{type(e).__name__}: {e}"

def fresh_db(path):
    if os.path.exists(path):
        os.remove(path)
    con = sqlite3.connect(path)
    con.executescript("""
      CREATE TABLE item (id INTEGER PRIMARY KEY, fingerprint TEXT UNIQUE, subject TEXT, sender TEXT,
        date TEXT, body TEXT, msg_class TEXT, att_count INTEGER, raw_headers TEXT);
      CREATE TABLE occurrence (item_id INTEGER, pst TEXT, folder_path TEXT, identifier TEXT,
        PRIMARY KEY (item_id, pst, identifier));
      CREATE TABLE pst_log (pst TEXT PRIMARY KEY, sha256 TEXT, items INTEGER, elapsed_s REAL);
      CREATE INDEX occ_pst ON occurrence(pst);
    """)
    con.commit()
    return con

def load(con, pst, items):
    new = dup = 0
    for it in items:
        fp = item_fingerprint(it)
        row = con.execute("SELECT id FROM item WHERE fingerprint=?", (fp,)).fetchone()
        if row:
            iid = row[0]; dup += 1
        else:
            cur = con.execute("""INSERT INTO item (fingerprint,subject,sender,date,body,msg_class,att_count,raw_headers)
                                 VALUES (?,?,?,?,?,?,?,?)""",
                              (fp, it["subject"], it["sender"], it["date"], it["body"], it["msg_class"],
                               it["att_count"], it["raw_headers"]))
            iid = cur.lastrowid; new += 1
        con.execute("INSERT OR IGNORE INTO occurrence VALUES (?,?,?,?)",
                    (iid, it["pst"], it["folder_path"], it["identifier"]))
    con.execute("INSERT OR REPLACE INTO pst_log VALUES (?,?,?,?)",
                (pst, "", len(items), 0.0))
    return new, dup

if __name__ == "__main__":
    psts = sorted(glob.glob(os.path.join(CORPUS, "*.pst")))
    print(f"corpus: {len(psts)} PST files, {sum(os.path.getsize(p) for p in psts)/1e6:.1f} MB total\n")
    mode = sys.argv[1] if len(sys.argv) > 1 else "all"

    if mode in ("all", "serial"):
        con = fresh_db(os.path.join(HERE, "bench_serial.sqlite"))
        t0 = time.time(); new = dup = 0
        for p in psts:
            n, d = load(con, os.path.basename(p), extract_one(p)); new += n; dup += d
        con.commit(); el = time.time() - t0
        print(f"[SERIAL 1 proc] {len(psts)} PSTs in {el:.2f}s -> "
              f"{len(psts)/el:.0f} files/s | unique items={new} dup occurrences={dup}")

    if mode in ("all", "parallel"):
        for nproc in (4, 8):
            con = fresh_db(os.path.join(HERE, f"bench_par{nproc}.sqlite"))
            t0 = time.time()
            with mp.Pool(nproc) as pool:
                for pst, items, err in pool.imap_unordered(_worker, psts):
                    if err:
                        print(f"   ERROR {pst}: {err}"); continue
                    load(con, pst, items)
            con.commit(); el = time.time() - t0
            uniq = con.execute("SELECT COUNT(*) FROM item").fetchone()[0]
            occ = con.execute("SELECT COUNT(*) FROM occurrence").fetchone()[0]
            print(f"[PARALLEL {nproc} proc] {len(psts)} PSTs in {el:.2f}s -> "
                  f"{len(psts)/el:.0f} files/s | unique items={uniq} occurrences={occ}")
            con.close()

    if mode in ("all", "dedupe"):
        con = fresh_db(os.path.join(HERE, "bench_dedupe.sqlite"))
        t0 = time.time(); new = dup = 0
        for p in psts:
            n, d = load(con, os.path.basename(p), extract_one(p)); new += n; dup += d
        con.commit()
        print(f"\n[DEDUPE] {len(psts)} PSTs -> {new} unique items, {dup} duplicate occurrences "
              f"collapsed in {time.time()-t0:.2f}s")
        for r in con.execute("""SELECT i.subject, COUNT(o.pst) AS n_psts FROM item i
                                JOIN occurrence o ON o.item_id=i.id
                                GROUP BY i.id ORDER BY n_psts DESC"""):
            print(f"   {r[1]:>4} PSTs contain {r[0]!r}")
        print("   sample provenance for item 1:",
              [tuple(x) for x in con.execute("SELECT pst, identifier FROM occurrence WHERE item_id=1 LIMIT 3")])
        con.close()
