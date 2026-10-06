"""Fail-soft libpff/pypff audit: how far does libpff get, and where exactly does it fail?"""
import sys, time
import pypff

path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/mailbox-100mb.pst"
t0 = time.time()
pst = pypff.file()
pst.open(path)

found = {"folders": 0, "messages": 0, "attachments": 0, "ok_folders": 0}
errors = []
samples = []


def walk(folder, path_acc, depth=0):
    counts = None
    try:
        counts = folder.number_of_sub_messages
    except Exception as e:
        errors.append((path_acc, f"number_of_sub_messages: {type(e).__name__}: {str(e)[:110]}"))
    name = "?"
    try:
        name = folder.get_name() or "(root)"
    except Exception as e:
        errors.append((path_acc, f"get_name: {type(e).__name__}"))
    full = f"{path_acc}/{name}".strip("/")
    found["folders"] += 1

    if counts is not None:
        for i in range(counts):
            try:
                m = folder.get_sub_message(i)
                found["messages"] += 1
                found["attachments"] += m.get_number_of_attachments()
                if len(samples) < 4:
                    rcpts = None
                    try:
                        rcpts = m.recipients
                    except Exception:
                        pass
                    samples.append((full, str(m.get_subject())[:44], str(m.get_sender_name()),
                                    len(rcpts) if rcpts else 0, m.get_number_of_attachments()))
            except Exception as e:
                errors.append((full, f"get_sub_message({i}): {type(e).__name__}: {str(e)[:110]}"))
                break
        found["ok_folders"] += 1

    subs = None
    try:
        subs = folder.number_of_sub_folders
    except Exception as e:
        errors.append((full, f"number_of_sub_folders: {type(e).__name__}: {str(e)[:130]}"))
    if subs:
        for i in range(subs):
            try:
                walk(folder.get_sub_folder(i), full, depth + 1)
            except Exception as e:
                errors.append((full, f"get_sub_folder({i}): {type(e).__name__}: {str(e)[:130]}"))


walk(pst.get_root_folder(), "")
pst.close()
el = time.time() - t0

print(f"libpff/pypff on {path}  ({el:.2f}s)")
print(f"  folders visited={found['folders']} readable={found['ok_folders']} "
      f"messages={found['messages']} attachments={found['attachments']}")
if found["messages"]:
    print(f"  partial throughput: {found['messages']/el:,.0f} messages/s")
print(f"  errors: {len(errors)}")
for where, err in errors[:8]:
    print(f"    at {where!r}\n      {err}")
print("\n  samples:")
for s in samples:
    print(f"    {s[0]} | subj={s[1]!r} from={s[2]!r} rcpt={s[3]} atts={s[4]}")
