//! Generate a realistic synthetic PST mailbox of a target size.
//!
//! Uses the write-capable fork of Microsoft's PST crate (outlook-pst-rw) to create a
//! Unicode PST and append messages. Output is shaped like a Purview-style mailbox
//! export: Zipf-skewed vocabulary (realistic search selectivity), file attachments,
//! To/Cc/Bcc recipient tables, multi-level folders, duplicated messages across
//! folders, and some CJK/UTF-8 subjects to exercise tokenisation and font handling.
//!
//! All content is generated. No real mail, people or domains are involved.
//!
//! Usage: pstgen out.pst [--messages N] [--attach-every K] [--attach-kb K] [--dup-every K]

use outlook_pst::{
    UnicodePstAttachment, UnicodePstBatchMessage, UnicodePstFile, UnicodePstMessage,
    UnicodePstRecipient, UnicodePstRecipientType,
};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

const WORDS: &[&str] = &[
    "invoice", "contract", "agreement", "payment", "remittance", "wire", "transfer",
    "schedule", "budget", "forecast", "variance", "meeting", "minutes", "action", "owner",
    "deadline", "counterparty", "amendment", "settlement", "audit", "compliance", "review",
    "draft", "final", "attachment", "statement", "reconciliation", "accrual", "provision",
    "termination", "renewal", "clause", "liability", "indemnity", "confidential", "privileged",
    "trading", "volume", "nomination", "pipeline", "capacity", "hedge", "position", "exposure",
    "quarterly", "annual", "board", "committee", "regulator", "disclosure", "material",
    "urgent", "please", "confirm", "attached", "forwarded", "clarification", "outstanding",
    "escalation", "approval", "signoff", "drafting", "redline", "comment", "tracking",
];
const SURNAMES: &[&str] = &[
    "Avery", "Baxter", "Chen", "Duval", "Ellison", "Farrow", "Grady", "Hollis", "Ibrahim",
    "Jensen", "Kowalski", "Lambert", "Mercer", "Napier", "Okafor", "Petrov", "Quinn", "Reyes",
    "Sandoval", "Tremaine", "Ulrich", "Vance", "Whitlock", "Xu", "Yates", "Zamora",
];
const FIRSTS: &[&str] = &[
    "Alex", "Bella", "Cameron", "Dana", "Evan", "Fiona", "Grace", "Hugo", "Imogen", "Jonas",
    "Kim", "Lena", "Marcus", "Nadia", "Oscar", "Priya", "Rafael", "Sofia", "Theo", "Uma",
];
const DOMAINS: &[&str] = &[
    "northgate.example", "lumenpartners.example", "harborcap.example", "meridian-trade.example",
];
const FOLDERS: &[&[&str]] = &[
    &["Inbox"],
    &["Sent Items"],
    &["Deleted Items"],
    &["Projects", "2021 Northgate"],
    &["Projects", "2022 Settlement"],
    &["Projects", "2023 Audit"],
    &["Legal Holds", "Review"],
];
const ATTACH_NAMES: &[&str] = &[
    "statement", "invoice", "redline", "schedule", "reconciliation", "ledger", "nomination",
];
const ATTACH_EXTS: &[&str] = &["pdf", "xlsx", "png", "txt"];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
}

/// Zipf-ish word draw: a few very common words, a long rare tail -> realistic
/// search selectivity (most queries should NOT match most of the corpus).
fn zipf_word(rng: &mut Rng) -> &'static str {
    let n = WORDS.len();
    loop {
        let rank = rng.below(n);
        let accept = (n as f64) / ((rank + 1) as f64 * 6.0);
        if accept >= 1.0 {
            return WORDS[rank];
        }
        if (rng.next() % 10_000) as f64 / 10_000.0 < accept {
            return WORDS[rank];
        }
    }
}

fn sentence(rng: &mut Rng, words: usize) -> String {
    let mut s = String::new();
    for i in 0..words {
        let w = zipf_word(rng);
        if i == 0 {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                s.push_str(&f.to_uppercase().to_string());
                s.push_str(c.as_str());
            }
        } else {
            s.push_str(w);
        }
        if i + 1 < words {
            s.push(' ');
        }
    }
    s.push('.');
    s
}

fn make_body(rng: &mut Rng, paragraphs: usize) -> String {
    let mut out = String::new();
    for p in 0..paragraphs {
        for _ in 0..rng.range(4, 9) {
            let n = rng.range(8, 22);
            out.push_str(&sentence(rng, n));
            out.push(' ');
        }
        out.push_str("\r\n\r\n");
        if p == 0 && rng.below(4) == 0 {
            out.push_str(
                "This message and any attachments are confidential and may be privileged. \
                 If you are not the intended recipient, notify the sender and delete it.\r\n\r\n",
            );
        }
    }
    out
}

fn person(rng: &mut Rng) -> (String, String) {
    let first = FIRSTS[rng.below(FIRSTS.len())];
    let last = SURNAMES[rng.below(SURNAMES.len())];
    let domain = DOMAINS[rng.below(DOMAINS.len())];
    (
        format!("{first} {last}"),
        format!("{}.{}@{domain}", first.to_lowercase(), last.to_lowercase()),
    )
}

fn filetime(unix_secs: i64) -> i64 {
    (unix_secs + 11_644_473_600) * 10_000_000
}

fn fake_file(rng: &mut Rng, kind: usize, size: usize) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(size + 64);
    match kind % 4 {
        0 => v.extend_from_slice(b"%PDF-1.7\n"),
        1 => v.extend_from_slice(b"PK\x03\x04"),
        2 => v.extend_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
        _ => {}
    }
    while v.len() < size {
        if kind % 4 == 3 || v.len() % 7 == 0 {
            let n = rng.range(8, 20);
            v.extend_from_slice(sentence(rng, n).as_bytes());
            v.push(b'\n');
        } else {
            v.push((rng.next() & 0xFF) as u8);
        }
        if v.len() > size + 4096 {
            break;
        }
    }
    v.truncate(size);
    v
}

#[derive(Clone)]
struct Gen {
    subject: String,
    sender_name: String,
    sender_email: String,
    rcpt: Vec<(String, String, u8)>, // name, email, kind (0=To,1=Cc,2=Bcc)
    body: String,
    html: String,
    message_id: String,
    delivery_time: i64,
    attachments: Vec<(String, String, Vec<u8>)>, // filename, mime, data
}

fn generate(rng: &mut Rng, i: usize, attach_every: usize, attach_kb: usize, counters: &mut (usize, usize, usize)) -> Gen {
    let (sender_name, sender_email) = person(rng);
    let n_rcpt = rng.range(1, 4);
    let mut rcpt = Vec::with_capacity(n_rcpt);
    for r in 0..n_rcpt {
        let (name, email) = person(rng);
        let kind = match r {
            0 => 0u8,
            1 => 1,
            _ => {
                if rng.below(3) == 0 {
                    2
                } else {
                    1
                }
            }
        };
        rcpt.push((name, email, kind));
    }
    let subject: String = match rng.below(40) {
        0 => "Re: 契約条件の確認について".to_string(),
        1 => "Überprüfung der Abrechnung — Rückfrage".to_string(),
        2 => "RE: 監査資料の提出期限".to_string(),
        _ => {
            let n = rng.range(4, 9);
            let mut s = String::new();
            if rng.below(3) == 0 {
                s.push_str("RE: ");
            }
            for k in 0..n {
                s.push_str(zipf_word(rng));
                if k + 1 < n {
                    s.push(' ');
                }
            }
            s
        }
    };
    let paras = rng.range(2, 7);
    let body = make_body(rng, paras);
    let html = format!(
        "<html><body><pre>{}</pre></body></html>",
        body.replace('&', "&amp;").replace('<', "&lt;")
    );
    let message_id = format!("<gen-{i:08}@{}>", DOMAINS[i % DOMAINS.len()]);
    let day = rng.range(0, 2_920) as i64;
    let secs = 1_514_764_800 + day * 86_400 + rng.range(0, 86_399) as i64;

    let mut attachments = Vec::new();
    if i % attach_every == 0 || rng.below(25) == 0 {
        let n = rng.range(1, 2);
        for a in 0..n {
            let size = attach_kb * 1024 / (a + 1);
            let fname = format!(
                "{}_{a}.{}",
                ATTACH_NAMES[rng.below(ATTACH_NAMES.len())],
                ATTACH_EXTS[rng.below(ATTACH_EXTS.len())]
            );
            let mime = match fname.rsplit('.').next().unwrap_or("bin") {
                "pdf" => "application/pdf",
                "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "png" => "image/png",
                _ => "text/plain",
            };
            let data = fake_file(rng, i + a, size);
            counters.1 += data.len();
            attachments.push((fname, mime.to_string(), data));
        }
    }
    counters.0 += body.len();
    if !attachments.is_empty() {
        counters.2 += 1;
    }
    Gen { subject, sender_name, sender_email, rcpt, body, html, message_id, delivery_time: filetime(secs), attachments }
}

fn append_batch(path: &Path, folder: &[&str], gen: &[Gen]) -> io::Result<()> {
    // Borrowed views over data owned by `gen` - no leaks, nothing outlives the call.
    let rcpt_views: Vec<Vec<UnicodePstRecipient>> = gen
        .iter()
        .map(|g| {
            g.rcpt
                .iter()
                .map(|(n, e, k)| UnicodePstRecipient {
                    name: n,
                    email: e,
                    recipient_type: match k {
                        0 => UnicodePstRecipientType::To,
                        1 => UnicodePstRecipientType::Cc,
                        _ => UnicodePstRecipientType::Bcc,
                    },
                })
                .collect()
        })
        .collect();
    let att_views: Vec<Vec<UnicodePstAttachment>> = gen
        .iter()
        .map(|g| {
            g.attachments
                .iter()
                .map(|(f, m, d)| UnicodePstAttachment {
                    filename: f,
                    mime_type: m,
                    content_id: None,
                    data: d,
                })
                .collect()
        })
        .collect();
    let batch: Vec<UnicodePstBatchMessage> = gen
        .iter()
        .enumerate()
        .map(|(i, g)| UnicodePstBatchMessage {
            message: UnicodePstMessage {
                subject: &g.subject,
                sender_name: &g.sender_name,
                sender_email: &g.sender_email,
                recipients: &rcpt_views[i],
                body: &g.body,
                html_body: Some(&g.html),
                message_id: &g.message_id,
                delivery_time: g.delivery_time,
            },
            attachments: &att_views[i],
        })
        .collect();
    drop(UnicodePstFile::append_many_in_folder_with_attachments(path, folder, &batch)?);
    Ok(())
}

fn append_batch_root(path: &Path, gen: &[Gen]) -> io::Result<()> {
    let rcpt_views: Vec<Vec<UnicodePstRecipient>> = gen
        .iter()
        .map(|g| {
            g.rcpt
                .iter()
                .map(|(n, e, k)| UnicodePstRecipient {
                    name: n,
                    email: e,
                    recipient_type: match k {
                        0 => UnicodePstRecipientType::To,
                        1 => UnicodePstRecipientType::Cc,
                        _ => UnicodePstRecipientType::Bcc,
                    },
                })
                .collect()
        })
        .collect();
    let att_views: Vec<Vec<UnicodePstAttachment>> = gen
        .iter()
        .map(|g| {
            g.attachments
                .iter()
                .map(|(f, m, d)| UnicodePstAttachment {
                    filename: f,
                    mime_type: m,
                    content_id: None,
                    data: d,
                })
                .collect()
        })
        .collect();
    let batch: Vec<UnicodePstBatchMessage> = gen
        .iter()
        .enumerate()
        .map(|(i, g)| UnicodePstBatchMessage {
            message: UnicodePstMessage {
                subject: &g.subject,
                sender_name: &g.sender_name,
                sender_email: &g.sender_email,
                recipients: &rcpt_views[i],
                body: &g.body,
                html_body: Some(&g.html),
                message_id: &g.message_id,
                delivery_time: g.delivery_time,
            },
            attachments: &att_views[i],
        })
        .collect();
    drop(UnicodePstFile::append_many(path, &batch.iter().map(|b| b.message).collect::<Vec<_>>())?);
    Ok(())
}

fn main() -> io::Result<()> {
    let argv: Vec<String> = env::args().collect();
    let path = argv
        .get(1)
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("generated.pst"));
    let flag = |name: &str, default: usize| -> usize {
        argv.iter()
            .position(|a| a == name)
            .and_then(|i| argv.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let messages = flag("--messages", 500);
    let attach_every = flag("--attach-every", 12).max(1);
    let attach_kb = flag("--attach-kb", 240);
    let dup_every = flag("--dup-every", 50).max(1);
    let batch = flag("--batch", 200);
    let flat = argv.iter().any(|a| a == "--flat");

    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut counters = (0usize, 0usize, 0usize); // body bytes, attach bytes, msgs w/ attach
    let t0 = Instant::now();

    // first message creates the file
    let first = generate(&mut rng, 0, attach_every, attach_kb, &mut counters);
    let first_atts: Vec<UnicodePstAttachment> = first
        .attachments
        .iter()
        .map(|(f, m, d)| UnicodePstAttachment { filename: f, mime_type: m, content_id: None, data: d })
        .collect();
    if first_atts.is_empty() {
        let seed: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let a = UnicodePstAttachment { filename: "seed.png", mime_type: "image/png", content_id: None, data: seed };
        drop(UnicodePstFile::create_with_attachments(&path, &UnicodePstMessage {
            subject: &first.subject,
            sender_name: &first.sender_name,
            sender_email: &first.sender_email,
            recipients: &[],
            body: &first.body,
            html_body: Some(&first.html),
            message_id: &first.message_id,
            delivery_time: first.delivery_time,
        }, &[a])?);
    } else {
        drop(UnicodePstFile::create_with_attachments(&path, &UnicodePstMessage {
            subject: &first.subject,
            sender_name: &first.sender_name,
            sender_email: &first.sender_email,
            recipients: &[],
            body: &first.body,
            html_body: Some(&first.html),
            message_id: &first.message_id,
            delivery_time: first.delivery_time,
        }, &first_atts)?);
    }
    drop(first);
    let mut written = 1usize;
    println!("created {}", path.display());

    let mut buf: Vec<Gen> = Vec::with_capacity(batch);
    let mut dup_buf: Vec<Gen> = Vec::new();
    for i in 1..messages {
        let g = generate(&mut rng, i, attach_every, attach_kb, &mut counters);
        // true duplicate: an exact copy of the same message, placed in another folder
        if !flat && i % dup_every == 0 {
            dup_buf.push(g.clone());
        }
        buf.push(g);
        if buf.len() >= batch || i == messages - 1 {
            if flat {
                let views: Vec<UnicodePstBatchMessage> = Vec::new();
                drop(views);
                append_batch_root(&path, &buf)?;
            } else {
                let folder = FOLDERS[(written / 137) % FOLDERS.len()];
                append_batch(&path, folder, &buf)?;
            }
            written += buf.len();
            buf.clear();
            if !flat && written % 1_000 < batch {
                println!(
                    "  {written} messages  {:.1}s  size {:.1} MB",
                    t0.elapsed().as_secs_f64(),
                    std::fs::metadata(&path)?.len() as f64 / 1e6
                );
            }
        }
    }

    // duplicated messages into another folder: same Message-ID and content,
    // second location. Exercises cross-folder dedupe with provenance retained.
    let dups = std::mem::take(&mut dup_buf);
    for chunk in dups.chunks(batch.max(1)) {
        if let Err(e) = append_batch(&path, &["Legal Holds", "Duplicates"], chunk) {
            eprintln!("duplicate append failed: {e}");
        } else {
            written += chunk.len();
        }
    }

    let size = std::fs::metadata(&path)?.len();
    println!(
        "\ndone\n  messages written : {} ({} with attachments)\n  duplicate copies : {}\n  file size        : {:.1} MB\n  body bytes       : {:.1} MB\n  attachment bytes : {:.1} MB\n  elapsed          : {:.1}s\n  bytes/message    : {:.0}",
        written,
        counters.2,
        dups.len(),
        size as f64 / 1e6,
        counters.0 as f64 / 1e6,
        counters.1 as f64 / 1e6,
        t0.elapsed().as_secs_f64(),
        size as f64 / written.max(1) as f64
    );
    Ok(())
}
