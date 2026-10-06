"""Demo: search the index and round-trip an export back through the mail parser."""
import sqlite3, os, glob, sys
sys.path.insert(0, "/home/hermes/.hermes/cache/scratch/pstprobe")
from indexer import search, export_eml, DB, EXPORT
from email import policy
from email.parser import BytesParser

print("=" * 78)
print("SEARCH DEMO")
print("=" * 78)
queries = ["alpha", "message AND gamma", "delta", '"IPM.Note"', "png", "subject:Alpha", "nonsenseword"]
for q in queries:
    rows = search(q)
    print(f"\nquery: {q!r}  -> {len(rows)} hit(s)")
    for r in rows:
        print(f"   id={r['id']} score={r['score']:.3f} class-subj={r['subject']!r} "
              f"atts={r['attachment_count']} folder={r['folder_path']!r}")
        print(f"      frag: {r['frag'][:100]}")

print("\n" + "=" * 78)
print("METADATA-ONLY / INDEXED FIELDS (no body search)")
print("=" * 78)
con = sqlite3.connect(DB); con.row_factory = sqlite3.Row
for r in con.execute("SELECT id, subject, message_class, attachment_count, body_chars, date FROM mail"):
    print(f"  id={r['id']} class={r['message_class']!r} atts={r['attachment_count']} "
          f"body_chars={r['body_chars']} date={r['date']} subj={r['subject']!r}")
cols = [d[0] for d in con.execute("SELECT * FROM mail LIMIT 0").description]
print("\nindexed columns:", cols)
print("FTS columns:", [r[0] for r in con.execute("PRAGMA table_info(mail_fts)")])
print("ingest log:", [tuple(r) for r in con.execute("SELECT pst, items, elapsed_s FROM ingest_log")])
con.close()

print("\n" + "=" * 78)
print("EXPORT DEMO (.eml round-trip)")
print("=" * 78)
for msg_id in [1, 2]:
    try:
        path, em = export_eml(msg_id)
        # round-trip: re-parse the file we just wrote, like a real mail client would
        with open(path, "rb") as fh:
            reparsed = BytesParser(policy=policy.default).parse(fh)
        print(f"\n  id={msg_id} -> {path}")
        print(f"    size on disk: {os.path.getsize(path)} bytes")
        print(f"    reparsed subject={str(reparsed['Subject'])!r} from={str(reparsed['From'])!r} "
              f"date={str(reparsed['Date'])!r} ctype={reparsed.get_content_type()}")
        body = reparsed.get_body(preferencelist=("plain",))
        print(f"    reparsed body: {body.get_content().strip()[:80]!r}" if body else "    (no body)")
        print(f"    content parts: {[p.get_content_type() for p in reparsed.walk()]}")
    except Exception as e:
        print(f"  id={msg_id} EXPORT FAILED: {type(e).__name__}: {e}")
