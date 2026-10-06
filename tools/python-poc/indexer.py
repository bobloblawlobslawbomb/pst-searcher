"""
PST -> SQLite FTS5 index -> search -> .eml export. Proof of concept.
Reads real PSTs, extracts items, indexes them, searches, and round-trips an export.
"""
import os, sys, glob, sqlite3, json, re
import html.parser
from datetime import datetime, timezone
from email.message import EmailMessage
from email import policy
from email.parser import BytesParser
import pypff

ROOT = "/home/hermes/.hermes/cache/scratch/pstprobe"
DB = os.path.join(ROOT, "index.sqlite")
EXPORT = os.path.join(ROOT, "eml_export")

# ---------- extraction ----------

def as_text(v):
    if v is None:
        return ""
    if isinstance(v, bytes):
        for enc in ("utf-8", "cp1252", "cp932", "latin-1"):
            try:
                return v.decode(enc)
            except UnicodeDecodeError:
                continue
        return v.decode("utf-8", "replace")
    return str(v)

TAG_RE = re.compile(r"<[^>]+>")
class _Strip(html.parser.HTMLParser):
    def __init__(self): super().__init__(); self.out = []
    def handle_data(self, d): self.out.append(d)

def html_to_text(h):
    p = _Strip()
    try:
        p.feed(h)
    except Exception:
        return TAG_RE.sub(" ", h)
    return re.sub(r"\n{3,}", "\n\n", "".join(p.out)).strip()

def parse_headers(raw):
    """Parse a transport-header blob into a dict."""
    if not raw:
        return {}
    txt = as_text(raw)
    if not txt.strip():
        return {}
    return dict(BytesParser(policy=policy.default).parsebytes(txt.encode("utf-8", "replace")).items())

def recipients_from(m):
    """pypff 'recipients' attr, else transport headers."""
    out = []
    r = getattr(m, "recipients", None)
    if r:
        for item in r:
            try:
                out.append({"name": as_text(getattr(item, "display_name", None)) or as_text(item),
                            "type": as_text(getattr(item, "type", None))})
            except Exception:
                out.append({"name": as_text(item), "type": ""})
    return out

def prop_from_record_sets(m, entry_type, max_sets=4):
    """Read a named MAPI property straight out of the record sets.
    pypff exposes no PR_MESSAGE_CLASS accessor; this recovers it.
    0x001A = PR_MESSAGE_CLASS, 0x0E04 = PR_DISPLAY_TO, 0x0E03 = PR_DISPLAY_CC."""
    for i in range(min(m.get_number_of_record_sets(), max_sets)):
        try:
            rs = m.get_record_set(i)
            for j in range(rs.get_number_of_entries()):
                e = rs.get_entry(j)
                if e.get_entry_type() == entry_type:
                    try:
                        return as_text(e.get_data_as_string())
                    except Exception:
                        try:
                            return as_text(e.get_data())
                        except Exception:
                            return ""
        except Exception:
            continue
    return ""

def walk(folder, path, pst_name, out, depth=0):
    name = folder.get_name() or "(root)"
    full = f"{path}/{name}".strip("/")
    for i in range(folder.number_of_sub_messages):
        m = folder.get_sub_message(i)
        hdrs = parse_headers(m.get_transport_headers())
        body = as_text(m.get_plain_text_body())
        if not body.strip():
            hb = as_text(m.get_html_body())
            body = html_to_text(hb) if hb.strip() else ""
        atts = []
        for j in range(m.number_of_attachments):
            a = m.get_attachment(j)
            atts.append({"name": as_text(a.get_long_filename()) or f"attachment{j}",
                         "size": a.get_size()})
        out.append({
            "pst": pst_name,
            "folder_path": full,
            "identifier": m.get_identifier(),
            "subject": as_text(m.get_subject()),
            "sender_name": as_text(m.get_sender_name()),
            "sender_email": hdrs.get("From", ""),
            "to": hdrs.get("To", "") or hdrs.get("X-Original-To", ""),
            "cc": hdrs.get("Cc", ""),
            "date": as_text(m.get_delivery_time()) or as_text(m.get_client_submit_time()),
            "date_iso": (m.get_delivery_time() or m.get_client_submit_time() or
                         m.get_creation_time()).isoformat() if (m.get_delivery_time() or m.get_client_submit_time() or m.get_creation_time()) else "",
            "conversation_topic": as_text(m.get_conversation_topic()),
            "conversation_index": as_text(m.get_conversation_index()),
            "body": body,
            "html_body": as_text(m.get_html_body()),
            "rtf_body": as_text(m.get_rtf_body()),
            "raw_headers": as_text(m.get_transport_headers()),
            "recipients": json.dumps(recipients_from(m), ensure_ascii=False),
            "attachments": json.dumps(atts, ensure_ascii=False),
            "attachment_count": m.get_number_of_attachments(),
            "attachment_names": " ".join(a["name"] for a in atts),
            "attachment_bytes": sum(a["size"] or 0 for a in atts),
            # pypff exposes no message size accessor; index body length instead
            "body_chars": len(body),
            "message_class": prop_from_record_sets(m, 0x001A),
        })
    for i in range(folder.number_of_sub_folders):
        walk(folder.get_sub_folder(i), full, pst_name, out, depth + 1)

# ---------- index ----------

SCHEMA = """
DROP TABLE IF EXISTS mail;
CREATE TABLE mail (
    id INTEGER PRIMARY KEY,
    pst TEXT, folder_path TEXT, identifier TEXT, subject TEXT,
    sender_name TEXT, sender_email TEXT, "to" TEXT, cc TEXT,
    date TEXT, date_iso TEXT, conversation_topic TEXT, conversation_index TEXT,
    body TEXT, html_body TEXT, rtf_body TEXT, raw_headers TEXT,
    recipients TEXT, attachments TEXT, attachment_count INTEGER,
    attachment_bytes INTEGER, body_chars INTEGER, message_class TEXT, item_hash TEXT
);
DROP TABLE IF EXISTS mail_fts;
CREATE VIRTUAL TABLE mail_fts USING fts5(
    subject, sender_name, sender_email, "to", cc, body, attachment_names,
    folder_path, conversation_topic,
    tokenize='unicode61 remove_diacritics 2'
);
DROP TABLE IF EXISTS ingest_log;
CREATE TABLE ingest_log (
    pst TEXT, sha256 TEXT, items INTEGER, indexed_at TEXT, elapsed_s REAL, UNIQUE(sha256)
);
"""

def ingest(pst_paths):
    import hashlib, time
    con = sqlite3.connect(DB)
    con.executescript(SCHEMA)
    total = 0
    for path in pst_paths:
        sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
        row = con.execute("SELECT 1 FROM ingest_log WHERE sha256=?", (sha,)).fetchone()
        if row:
            print(f"skip (already ingested) {os.path.basename(path)}")
            continue
        t0 = time.time()
        pst = pypff.file(); pst.open(path)
        items = []
        walk(pst.get_root_folder(), "", os.path.basename(path), items)
        pst.close()
        for it in items:
            it["attachment_names"] = it.get("attachment_names", "")
            cols = [c for c in it if c not in ("attachments", "attachment_names")]
            cur = con.execute(
                f'INSERT INTO mail ({",".join(chr(34)+c+chr(34) for c in cols)}) '
                f'VALUES ({",".join("?" * len(cols))})', [it[c] for c in cols])
            rowid = cur.lastrowid
            con.execute("""INSERT INTO mail_fts(rowid, subject, sender_name, sender_email, "to", cc,
                           body, attachment_names, folder_path, conversation_topic)
                           VALUES (?,?,?,?,?,?,?,?,?,?)""",
                        (rowid, it["subject"], it["sender_name"], it["sender_email"], it["to"], it["cc"],
                         it["body"], it["attachment_names"], it["folder_path"], it["conversation_topic"]))
        con.execute("INSERT INTO ingest_log VALUES (?,?,?,?,?)",
                    (os.path.basename(path), sha, len(items),
                     datetime.now(timezone.utc).isoformat(), round(time.time() - t0, 2)))
        con.commit()
        total += len(items)
        print(f"ingested {os.path.basename(path)}: {len(items)} items in {time.time()-t0:.2f}s")
    con.close()
    return total

def search(query, limit=10, **filters):
    con = sqlite3.connect(DB)
    con.row_factory = sqlite3.Row
    where, args = [], []
    if filters.get("after"):  where.append("m.date_iso >= ?"); args.append(filters["after"])
    if filters.get("before"): where.append("m.date_iso <= ?"); args.append(filters["before"])
    if filters.get("folder"): where.append("m.folder_path LIKE ?"); args.append(f"%{filters['folder']}%")
    if filters.get("has_attachments"): where.append("m.attachment_count > 0")
    clause = (" AND " + " AND ".join(where)) if where else ""
    sql = f"""SELECT m.id, m.subject, m.sender_name, m.sender_email, m.date, m.folder_path,
                     m.attachment_count, m.body_chars, bm25(mail_fts) AS score,
                     snippet(mail_fts, 6, '[', ']', ' … ', 12) AS frag
              FROM mail_fts JOIN mail m ON m.id = mail_fts.rowid
              WHERE mail_fts MATCH ?{clause}
              ORDER BY score LIMIT ?"""
    rows = con.execute(sql, [query, *args, limit]).fetchall()
    con.close()
    return rows

def export_eml(msg_id, outdir=EXPORT):
    con = sqlite3.connect(DB); con.row_factory = sqlite3.Row
    r = con.execute("SELECT * FROM mail WHERE id=?", (msg_id,)).fetchone()
    con.close()
    os.makedirs(outdir, exist_ok=True)
    em = EmailMessage()
    em["Subject"] = r["subject"] or "(no subject)"
    em["From"] = r["sender_email"] or r["sender_name"] or "unknown"
    em["To"] = r["to"] or ""
    if r["cc"]:
        em["Cc"] = r["cc"]
    em["Date"] = r["date"]
    em["X-PST-Source"] = f"{r['pst']}:{r['folder_path']}:{r['identifier']}"
    if r["html_body"] and r["html_body"].strip():
        em.set_content(r["body"] or "")
        em.add_alternative(r["html_body"], subtype="html")
    else:
        em.set_content(r["body"] or "")
    safe = re.sub(r"[^A-Za-z0-9._-]+", "_", (r["subject"] or "no_subject"))[:60]
    path = os.path.join(outdir, f"{r['id']:06d}_{safe}.eml")
    with open(path, "wb") as fh:
        fh.write(em.as_bytes(policy=policy.SMTP))
    return path, em

if __name__ == "__main__":
    psts = sorted(glob.glob(os.path.join(ROOT, "*.pst")))
    n = ingest(psts)
    con = sqlite3.connect(DB)
    print(f"\n=== indexed rows: {con.execute('SELECT COUNT(*) FROM mail').fetchone()[0]} "
          f"(this run added {n}) ===")
    print("folders:", [r[0] for r in con.execute("SELECT DISTINCT folder_path FROM mail")])
    con.close()
