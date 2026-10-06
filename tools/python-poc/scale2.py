"""Realistic-scale index test: Zipfian vocabulary (like real mail), plus a tested
FTS5 query escaper. Writes results incrementally so progress survives a timeout."""
import os, sqlite3, random, re, sys, time

DB = "/home/hermes/.hermes/cache/scratch/pstprobe/scale2.sqlite"
OUT = "/home/hermes/.hermes/cache/scratch/pstprobe/scale2_results.txt"
for p in (DB, DB + "-wal", DB + "-shm", OUT):
    if os.path.exists(p):
        os.remove(p)

def say(msg):
    print(msg, flush=True)
    with open(OUT, "a") as fh:
        fh.write(msg + "\n")

# ---------------- FTS5 query escaper (the thing that broke on '@') ----------------
OPS = {"AND", "OR", "NOT", "NEAR"}
FIELD_RE = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*):(.*)$", re.S)

def escape_fts_query(q: str) -> str:
    """Turn free text into a valid FTS5 MATCH string.
    Quotes every token containing non-word characters (@ . - / + etc), keeps
    AND/OR/NOT operators, and preserves field:value scoping with the value quoted."""
    parts, i = [], 0
    tokens = re.findall(r'"[^"]*"|\S+', q)
    for tok in tokens:
        if tok.startswith('"') and tok.endswith('"') and len(tok) > 1:
            parts.append(tok); continue
        if tok.upper() in OPS:
            parts.append(tok.upper()); continue
        m = FIELD_RE.match(tok)
        if m and m.group(1).lower() not in ("and", "or", "not"):
            field, val = m.group(1), m.group(2)
            if val.startswith("*") or val.endswith("*"):
                parts.append(f'{field}:"{val.strip(chr(42))}"*'); continue
            parts.append(f'{field}:"{val.replace(chr(34), chr(34)*2)}"')
            continue
        if re.fullmatch(r"[\w\u00c0-\uffff*]+", tok, re.UNICODE):
            parts.append(tok)
        else:
            parts.append('"' + tok.replace('"', '""') + '"')
    return " ".join(parts) if parts else '""'

say("=== query escaper self-test ===")
tests = ["invoice", "sender_email:user42@example.com", "2020-01-01", 'he said "hi"',
         "invoice AND contract", "subject:termination", "wire-transfer", "c++", "don't",
         "contr*", "audit NOT review"]
for t in tests:
    say(f"  in : {t!r}\n  out: {escape_fts_query(t)!r}")
say("")

# ---------------- build a realistically-skewed corpus ----------------
random.seed(7)
VOCAB_N = 60_000
vocab = ["".join(random.choices("abcdefghijklmnopqrstuvwxyz", k=random.randint(4, 10)))
         for _ in range(VOCAB_N)]
vocab = list(dict.fromkeys(vocab))
# Zipf-ish: ranks 0..N, weight 1/(rank+1)  -> a few very common words, long rare tail
weights = [1.0 / (i + 1) for i in range(len(vocab))]
# Precompute cumulative weights once: passing cum_weights makes random.choices
# O(k log n) per call instead of rebuilding an O(vocab) table for every row.
_cum, _tot = [], 0.0
for _w in weights:
    _tot += _w
    _cum.append(_tot)
CUM = _cum
N = 1_000_000
say(f"corpus: {N:,} items, vocabulary {len(vocab):,} terms (Zipf-skewed)")

con = sqlite3.connect(DB)
con.executescript("""
PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
CREATE TABLE item (id INTEGER PRIMARY KEY, pst TEXT, folder_path TEXT, subject TEXT,
  sender TEXT, sender_email TEXT, rcpt TEXT, date_iso TEXT, body TEXT, msg_class TEXT,
  att_count INTEGER, fingerprint TEXT);
CREATE VIRTUAL TABLE fts USING fts5(subject, sender, sender_email, rcpt, body,
  attachment_names, folder_path, tokenize='unicode61 remove_diacritics 2');
""")
t0 = time.time()
con.execute("BEGIN")
for i in range(N):
    words = random.choices(vocab, cum_weights=CUM, k=140)
    subj = " ".join(words[:4])
    snd = f"user{i % 500}@example.com"
    rcpt = f"user{random.randint(0, 499)}@example.com"
    body = " ".join(words)
    pst = f"custodian{i % 400:03d}_chunk{i % 13:02d}.pst"
    folder = f"Exchange/Custodian{i % 400}/Top of Information Store/{random.choice(['Inbox','Sent Items','Deleted Items','Projects'])}"
    date = f"20{random.randint(10,25):02d}-{random.randint(1,12):02d}-{random.randint(1,28):02d}"
    cur = con.execute("INSERT INTO item (pst,folder_path,subject,sender,sender_email,rcpt,date_iso,body,msg_class,att_count,fingerprint)"
                      " VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                      (pst, folder, subj, snd.split("@")[0], snd, rcpt, date, body, "IPM.Note",
                       random.choice([0,0,0,1,2]), f"fp{i}"))
    con.execute("INSERT INTO fts (rowid,subject,sender,sender_email,rcpt,body,attachment_names,folder_path)"
                " VALUES (?,?,?,?,?,?,?,?)",
                (cur.lastrowid, subj, snd.split("@")[0], snd, rcpt, body, "", folder))
con.commit()
con.execute("ANALYZE"); con.commit()
say(f"built in {time.time()-t0:.0f}s ({N/(time.time()-t0):,.0f} items/s) | "
    f"DB {os.path.getsize(DB)/1e6:,.0f} MB | {con.execute('SELECT COUNT(DISTINCT pst) FROM item').fetchone()[0]:,} source PSTs")

# ---------------- selectivity vs latency ----------------
def run(label, raw_query, limit=10):
    q = escape_fts_query(raw_query)
    t1 = time.time()
    rows = con.execute("""SELECT fts.rowid, bm25(fts) s, snippet(fts,4,'[',']',' ... ',8)
                          FROM fts JOIN item ON item.id=fts.rowid
                          WHERE fts MATCH ? ORDER BY s LIMIT ?""", (q, limit)).fetchall()
    t2 = time.time()
    n = con.execute("SELECT COUNT(*) FROM fts JOIN item ON item.id=fts.rowid WHERE fts MATCH ?", (q,)).fetchone()[0]
    t3 = time.time()
    say(f"{label:<22}{n:>9,}{(t2-t1)*1000:>9.1f}{(t3-t2)*1000:>12.1f}   {(rows[0][2][:40] if rows else '-')}")
    return n

say(f"\n{'query':<22}{'hits':>9}{'top10 ms':>9}{'count ms':>12}   snippet")
common, mid, rare = " ".join(vocab[:1]), " ".join(vocab[2000:2001]), " ".join(vocab[-1:])
most_common = ""
run("very common term", common)
run("mid-frequency term", mid)
run("rare term (1 in corpus)", rare)
run("phantom term", "zzzznotpresent")
run("two rare AND", f"{rare} AND {mid}")
run("phrase (3 words)", '"' + " ".join(words[:3]) + '"')
run("sender-scoped (@ char)", "sender_email:user42@example.com")
run("date-like token", "2020-01-01")
run("wildcard prefix", rare[:5] + "*")

t1 = time.time()
r = con.execute("""SELECT COUNT(*) FROM fts JOIN item ON item.id=fts.rowid
                   WHERE fts MATCH ? AND item.date_iso BETWEEN '2020-01-01' AND '2021-12-31'
                   AND item.att_count > 0 AND item.folder_path LIKE '%Inbox%'""",
                (escape_fts_query(mid),)).fetchone()[0]
say(f"\nfaceted (date+attachment+folder) on mid-freq term: {r:,} hits in {(time.time()-t1)*1000:.0f} ms")

# ---------------- concurrent read during write ----------------
import threading
def writer():
    c = sqlite3.connect(DB, timeout=60)
    c.execute("PRAGMA journal_mode=WAL")
    for _ in range(30_000):
        c.execute("INSERT INTO fts (subject,body,folder_path) VALUES (?,?,?)",
                  ("live ingest", " ".join(random.choices(vocab, cum_weights=CUM, k=140)), "live"))
    c.commit(); c.close()
w = threading.Thread(target=writer); w.start()
lats = []
while w.is_alive() and len(lats) < 400:
    t1 = time.time()
    con.execute("SELECT COUNT(*) FROM fts JOIN item ON item.id=fts.rowid WHERE fts MATCH ?",
                (escape_fts_query(rare),)).fetchone()
    lats.append((time.time() - t1) * 1000)
w.join()
lats.sort()
say(f"concurrent search during live ingest: {len(lats)} queries | p50 {lats[len(lats)//2]:.0f} ms | "
    f"p95 {lats[int(len(lats)*0.95)]:.0f} ms | max {lats[-1]:.0f} ms")

# ---------------- export path ----------------
t1 = time.time()
ids = [r[0] for r in con.execute("""SELECT item.id FROM fts JOIN item ON item.id=fts.rowid
                                    WHERE fts MATCH ? LIMIT 5000""", (escape_fts_query(mid),))]
t2 = time.time()
payload = con.execute(f"SELECT id, subject, body FROM item WHERE id IN ({','.join('?'*len(ids))})", ids).fetchall()
t3 = time.time()
say(f"\nexport prep: select {len(ids):,} ids {(t2-t1)*1000:.0f} ms | "
    f"read {sum(len(p[2]) for p in payload)/1e6:.1f} MB of bodies {(t3-t2)*1000:.0f} ms")
say(f"index total: {os.path.getsize(DB)/1e6:,.0f} MB for {N:,} items "
    f"({os.path.getsize(DB)/N:,.0f} bytes/item incl. full body + FTS)")
con.close()
