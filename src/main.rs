//! Probe: parse PST files with Microsoft's clean-room Rust PST crate.
//! Checks the two things pypff could not do: message class and recipients.
use std::error::Error;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::folder::Folder;
use outlook_pst::messaging::message::Message;
use outlook_pst::messaging::store::Store;
use outlook_pst::ndb::node_id::NodeId;

// MAPI property ids
const PR_MESSAGE_CLASS: u16 = 0x001A;
const PR_SUBJECT: u16 = 0x0037;
const PR_SUBJECT_W: u16 = 0x0037;
const PR_SENDER_NAME: u16 = 0x0C1A;
const PR_DISPLAY_TO: u16 = 0x0E04;
const PR_DISPLAY_CC: u16 = 0x0E03;
const PR_RECIPIENT_TYPE: u16 = 0x0C15;
const PR_SMTP_ADDRESS: u16 = 0x39FE;
const PR_RECIPIENT_DISPLAY_NAME: u16 = 0x3001;
const PR_EMAIL_ADDRESS: u16 = 0x3003;
const PR_ATTACH_LONG_FILENAME: u16 = 0x3704;
const PR_ATTACH_FILENAME: u16 = 0x3707;
const PR_ATTACH_SIZE: u16 = 0x0E20;
const PR_INTERNET_MESSAGE_ID: u16 = 0x1035;
const PR_BODY: u16 = 0x1000;
const PR_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;

#[derive(Default)]
struct Totals {
    folders: usize,
    messages: usize,
    with_class: usize,
    with_recipients: usize,
    with_attachments: usize,
    attachment_names: usize,
    with_msg_id: usize,
    with_body: usize,
}

fn dump_table(label: &str, table: &Rc<dyn TableContext>) {
    let context = table.context();
    for (i, row) in table.rows_matrix().enumerate() {
        match row.columns(context) {
            Ok(cols) => {
                for (column, value) in context.columns().iter().zip(cols) {
                    let Some(value) = value else { continue };
                    if let Ok(v) = table.read_column(&value, column.prop_type()) {
                        println!("        {label}[{i}] 0x{:04X} = {v:?}", column.prop_id());
                    }
                }
            }
            Err(e) => println!("        {label}[{i}] column read failed: {e:?}"),
        }
    }
}

fn walk(store: &Rc<dyn Store>, folder: &Rc<dyn Folder>, path: &str, depth: usize, t: &mut Totals) {
    let name = folder
        .properties()
        .display_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "(unnamed)".into());
    let full = format!("{path}/{name}");
    t.folders += 1;

    if let Some(contents) = folder.contents_table() {
        for row in contents.rows_matrix() {
            let Ok(entry_id) = store
                .properties()
                .make_entry_id(NodeId::from(u32::from(row.id())))
            else {
                continue;
            };
            let Ok(message) = store.open_message(&entry_id, None) else {
                continue;
            };
            t.messages += 1;
            let props = message.properties();
            let get = |id: u16| props.get(id).map(|v| format!("{v:?}"));

            let subject = get(PR_SUBJECT).or_else(|| get(PR_SUBJECT_W));
            let class = get(PR_MESSAGE_CLASS);
            if class.is_some() {
                t.with_class += 1;
            }
            if get(PR_INTERNET_MESSAGE_ID).is_some() {
                t.with_msg_id += 1;
            }
            if get(PR_BODY).is_some() {
                t.with_body += 1;
            }

            println!("{}[M] subj={:?} class={:?}", " ".repeat(depth), subject, class);
            println!("{}     sender={:?} to={:?} cc={:?}", " ".repeat(depth),
                     get(PR_SENDER_NAME), get(PR_DISPLAY_TO), get(PR_DISPLAY_CC));
            println!("{}     delivery={:?} msg_id={:?}", " ".repeat(depth),
                     get(PR_MESSAGE_DELIVERY_TIME), get(PR_INTERNET_MESSAGE_ID));

            if let Some(recipients) = message.recipient_table() {
                t.with_recipients += 1;
                println!("{}     recipients: {} row(s)", " ".repeat(depth), recipients.rows_matrix().count());
                dump_table("rcpt", recipients);
                let _ = (PR_RECIPIENT_TYPE, PR_SMTP_ADDRESS, PR_RECIPIENT_DISPLAY_NAME, PR_EMAIL_ADDRESS);
            }

            if let Some(attachments) = message.attachment_table() {
                t.with_attachments += 1;
                println!("{}     attachments: {} row(s)", " ".repeat(depth), attachments.rows_matrix().count());
                dump_table("att", attachments);
                for row in attachments.rows_matrix() {
                    let ctx = attachments.context();
                    if let Ok(cols) = row.columns(ctx) {
                        for (column, value) in ctx.columns().iter().zip(cols) {
                            if matches!(column.prop_id(), PR_ATTACH_LONG_FILENAME | PR_ATTACH_FILENAME | PR_ATTACH_SIZE) {
                                t.attachment_names += 1;
                            }
                            let _ = value;
                        }
                    }
                }
            }
        }
    }

    if let Some(hierarchy) = folder.hierarchy_table() {
        for row in hierarchy.rows_matrix() {
            if let Ok(entry_id) = store
                .properties()
                .make_entry_id(NodeId::from(u32::from(row.id())))
            {
                if let Ok(sub) = store.open_folder(&entry_id) {
                    walk(store, &sub, &full, depth + 1, t);
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: pst-rs-probe <file.pst> [more.pst ...]");
        std::process::exit(2);
    }
    let mut grand = Totals::default();
    for path in &args {
        println!("\n================ {} ================", path);
        let t0 = Instant::now();
        let store = outlook_pst::open_store(Path::new(path))?;
        let store_name = store.properties().display_name().map(|s| s.to_string());
        println!("store display_name: {store_name:?}");
        let ipm = store.properties().ipm_sub_tree_entry_id()?;
        let root = store.open_folder(&ipm)?;
        let mut t = Totals::default();
        walk(&store, &root, "", 0, &mut t);
        println!(
            "TOTALS: folders={} messages={} with_class={} with_recipients={} with_attachments={} \
             with_msg_id={} with_body={} elapsed={:.3}s",
            t.folders, t.messages, t.with_class, t.with_recipients, t.with_attachments,
            t.with_msg_id, t.with_body, t0.elapsed().as_secs_f64()
        );
        grand.folders += t.folders;
        grand.messages += t.messages;
        grand.with_class += t.with_class;
        grand.with_recipients += t.with_recipients;
        grand.with_attachments += t.with_attachments;
        grand.with_msg_id += t.with_msg_id;
        grand.with_body += t.with_body;
    }
    println!(
        "\nGRAND TOTAL across {} file(s): folders={} messages={} class={} recipients={} attachments={} msg_id={} body={}",
        args.len(), grand.folders, grand.messages, grand.with_class, grand.with_recipients,
        grand.with_attachments, grand.with_msg_id, grand.with_body
    );
    Ok(())
}
