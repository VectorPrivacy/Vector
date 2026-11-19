#!/usr/bin/env python3
"""On-screen regression checks for a devctl build, driven through `dev:` commands on an X display.

    cargo build --no-default-features --features tor,i2p,video,devctl --bin vector-gpui
    DISPLAY=:99 python3 scripts/onscreen.py [--bin PATH] [--fixtures FILE] [--only NAME] [--update]

Each check opens a screen from the fixtures and reads the app's own counters: a screen at rest must
draw nothing, hover and toast fades must end, a menu must close on Escape and fit the window when
opened in a corner, a chat at its newest message must follow arrivals while one scrolled up holds
still, a window in the background must stop presenting, and memory at rest must stay near the
recorded baseline (`--update` records it). Linux needs `xdotool` for focus and `xterm` for the background check; macOS brings Finder
forward for it.
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(HERE)
ANSWER = os.path.join(tempfile.gettempdir(), "vector-devctl.json")
BASELINE = os.path.join(CRATE, "reference", "onscreen-baseline.json")
W, H = 1000, 760
# Memory at rest may grow this much over the baseline before the check fails.
MEM_SLACK = 1.15

ap = argparse.ArgumentParser()
ap.add_argument("--bin", default=os.path.join(CRATE, "target", "debug", "vector-gpui"))
ap.add_argument("--fixtures", default=os.path.join(CRATE, "reference", "fixtures.json"))
ap.add_argument("--only")
ap.add_argument("--update", action="store_true")
args = ap.parse_args()
env = dict(os.environ, TZ="UTC")
results = []
MAC = sys.platform == "darwin"


def dev(cmd, wait=True):
    """Sends `dev:<cmd>`; with `wait`, returns the JSON the app writes back."""
    if wait:
        try:
            os.remove(ANSWER)
        except OSError:
            pass
    subprocess.run([args.bin, "dev:" + cmd], env=env, capture_output=True, timeout=30)
    if not wait:
        return None
    for _ in range(100):
        try:
            with open(ANSWER) as f:
                return json.load(f)
        except (OSError, ValueError):
            time.sleep(0.05)
    raise RuntimeError(f"no answer to dev:{cmd}")


def frames():
    return dev("state")["frames"]


def at_rest(settle=2.5, span=3.0):
    """Draws and presents over `span` seconds, once `settle` has let the last change land."""
    time.sleep(settle)
    frames()
    time.sleep(span)
    f = frames()
    return f["draws"], f["presents"]


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}  {detail}", flush=True)


def window_of(pid):
    """The X window `pid` opened. macOS needs no handle: the app raises itself."""
    if MAC:
        time.sleep(4)
        return "app"
    for _ in range(200):
        r = subprocess.run(["xdotool", "search", "--pid", str(pid)], env=env, capture_output=True, text=True)
        if r.stdout.split():
            return r.stdout.split()[-1]
        time.sleep(0.1)
    return None


def focus(wid):
    if MAC:
        dev("raise", wait=False)
    elif wid:
        subprocess.run(["xdotool", "windowfocus", wid], env=env, capture_output=True)
    time.sleep(0.5)


def a11y():
    """The accessibility tree, its bounds in window points."""
    tree = dev("a11y")
    scale = (tree.get("frame") or {}).get("scale_factor") or 1
    for n in tree.get("nodes", {}).values():
        if n.get("bounds"):
            n["bounds"] = [v / scale for v in n["bounds"]]
    return tree


def menus(tree):
    """Every menu and menu item in an accessibility tree."""
    return [n for n in tree.get("nodes", {}).values() if "menu" in n.get("aria", {}).get("role", "").lower()]


def wanted(name):
    return not args.only or args.only in name


def inside(node):
    """Whether a node has bounds and they lie within the window."""
    b = node.get("bounds")
    return b is not None and b[0] >= -0.5 and b[1] >= -0.5 and b[2] <= W + 0.5 and b[3] <= H + 0.5


def last_text():
    """A point on the lowest, rightmost text above the composer, or None."""
    texts = [n["bounds"] for n in a11y()["nodes"].values()
             if n.get("bounds") and n["aria"].get("role") == "Label" and n["bounds"][3] < H - 60 and n["bounds"][0] > W / 2]
    if not texts:
        return None
    b = max(texts, key=lambda b: b[2] + b[3])
    return round(b[2] - 3), round((b[1] + b[3]) / 2)


def arrive(chat, n=1):
    """`n` incoming messages in `chat`, as the core would announce them."""
    for _ in range(n):
        arrive.seq += 1
        msg = {"id": f"{arrive.seq:064x}", "content": f"Incoming {arrive.seq}", "replied_to": "", "preview_metadata": None,
               "attachments": [], "reactions": [], "at": int(time.time() * 1000), "pending": False, "failed": False,
               "mine": False, "npub": chat, "edited": False}
        dev("emit:message_new:" + json.dumps({"chat_id": chat, "message": msg}, separators=(",", ":")), wait=False)


arrive.seq = 0xC0FFEE


fx = json.load(open(args.fixtures))
chats = [c["id"] for c in fx.get("chats", []) if c.get("messages")][:6]
longest = max((c for c in fx.get("chats", []) if c.get("messages")), key=lambda c: len(c["messages"]), default=None)
communities = [c["community_id"] for c in fx.get("communities", [])][:2]

subprocess.run(["pkill", "-xf", f"{args.bin} --fixtures {args.fixtures} --size {W}x{H}"], capture_output=True)
log = tempfile.NamedTemporaryFile("w+", prefix="onscreen-", suffix=".log", delete=False)
app = subprocess.Popen([args.bin, "--fixtures", args.fixtures, "--size", f"{W}x{H}"], env=env, stdout=log, stderr=subprocess.STDOUT, cwd=CRATE)
try:
    wid = window_of(app.pid)
    focus(wid)
    time.sleep(2)
    check("boot", wid is not None and dev("state").get("screen") == "main", "window up, main screen")

    if wanted("rest:chats"):
        draws, presents = at_rest()
        check("rest:chats", draws == 0, f"draws={draws} presents={presents}")
    for i, chat in enumerate(chats):
        name = f"rest:chat{i}"
        if wanted(name):
            dev("chat:" + chat, wait=False)
            draws, presents = at_rest()
            shown = dev("state").get("open")
            check(name, draws == 0 and shown == chat, f"draws={draws} presents={presents} {chat[:16]}" + ("" if shown == chat else f" (open: {shown})"))
    for i, com in enumerate(communities):
        name = f"rest:community{i}"
        if wanted(name):
            dev("community:" + com, wait=False)
            draws, presents = at_rest()
            check(name, draws == 0, f"draws={draws} presents={presents}")

    if wanted("fade:hover") and chats:
        # A hover fades in once and then holds still: the pointer resting over a row draws nothing.
        dev("chat:" + chats[0], wait=False)
        time.sleep(1.5)
        at = last_text()
        frames()
        if at:
            dev(f"move:{at[0]},{at[1]}", wait=False)
        time.sleep(0.6)
        moved = frames()["draws"]
        draws, presents = at_rest()
        dev("move:5,5", wait=False)
        check("fade:hover", moved > 0 and draws == 0, f"at {at}: while fading={moved} then draws={draws} presents={presents}")

    if wanted("fade:toast"):
        # Held ~1.6 s, then faded out over 320 ms; the window is still once it has gone.
        frames()
        dev("emit:show_toast:" + json.dumps("On-screen check"), wait=False)
        time.sleep(0.6)
        shown = frames()["draws"]
        draws, presents = at_rest(settle=3)
        check("fade:toast", shown > 0 and draws == 0, f"while showing={shown} then draws={draws} presents={presents}")

    if wanted("menu:escape") and chats:
        dev("chat:" + chats[0], wait=False)
        time.sleep(1.5)
        dev(f"rclick:{W // 2 + 150},{H // 2}", wait=False)
        time.sleep(0.8)
        opened = len(menus(a11y()))
        dev("key:escape", wait=False)
        time.sleep(0.8)
        left = len(menus(a11y()))
        check("menu:escape", opened > 0 and left == 0, f"menus open={opened} after Escape={left}")

    if wanted("menu:clamp") and chats:
        # Opened on the lowest, rightmost text above the composer, a menu must still fit the window.
        at = last_text()
        if at:
            x, y = at
            dev(f"rclick:{x},{y}", wait=False)
            time.sleep(0.8)
            found = menus(a11y())
            out = [n for n in found if not inside(n)]
            dev("key:escape", wait=False)
            time.sleep(0.5)
            check("menu:clamp", found and not out, f"at {x},{y}: menus={len(found)} outside={len(out)}" + (f" first={out[0].get('bounds')}" if out else ""))
        else:
            check("menu:clamp", False, "no text to open a menu on")

    if longest and (wanted("scroll:follow") or wanted("scroll:hold")):
        chat = longest["id"]
        dev("chat:" + chat, wait=False)
        time.sleep(1.5)
        arrive(chat, 30)
        time.sleep(1.5)
        before = dev("state")
        arrive(chat)
        time.sleep(1.5)
        after = dev("state")
        if wanted("scroll:follow"):
            check("scroll:follow", before["following"] and after["following"] and after["timeline_items"] > before["timeline_items"],
                  f"following={before['following']}->{after['following']} items={before['timeline_items']}->{after['timeline_items']}")
        if wanted("scroll:hold"):
            dev(f"wheel:{W // 2 + 150},{H // 2},-300,2", wait=False)
            time.sleep(1.5)
            held = dev("state")
            arrive(chat, 3)
            time.sleep(1.5)
            now = dev("state")
            check("scroll:hold", not held["following"] and now["scroll"] == held["scroll"] and now["timeline_items"] > held["timeline_items"],
                  f"following={held['following']} scroll={held['scroll']}->{now['scroll']} items={held['timeline_items']}->{now['timeline_items']}")

    if wanted("background") and (MAC or shutil.which("xterm")) and wid:
        if chats:
            dev("chat:" + chats[0], wait=False)
        term = None
        try:
            if MAC:
                subprocess.run(["osascript", "-e", 'tell application "Finder" to activate'], capture_output=True)
            else:
                term = subprocess.Popen(["xterm", "-geometry", "20x4+0+0"], env=env)
                focus(window_of(term.pid))
            draws, presents = at_rest(settle=1.5)
            check("background", presents == 0 and draws <= 1, f"draws={draws} presents={presents}")
        finally:
            if term:
                term.kill()
            focus(wid)

    if wanted("memory"):
        if chats:
            dev("chat:" + chats[0], wait=False)
        time.sleep(6)
        m = dev("mem")
        # macOS: the kernel's footprint ledger. Linux: dirty private pages, as clean ones are mostly code.
        mb = m.get("footprint", m.get("dirty"))
        base = json.load(open(BASELINE)) if os.path.exists(BASELINE) else {}
        key = f"{sys.platform}-{'debug' if '/debug/' in os.path.abspath(args.bin) else 'release'}"
        if args.update or key not in base:
            base[key] = mb
            with open(BASELINE, "w") as f:
                json.dump(base, f, indent=1, sort_keys=True)
                f.write("\n")
            check("memory", True, f"{mb} MB recorded as the {key} baseline")
        else:
            check("memory", mb <= base[key] * MEM_SLACK, f"{mb} MB, baseline {base[key]} MB")
finally:
    app.terminate()
    try:
        app.wait(5)
    except subprocess.TimeoutExpired:
        app.kill()
    log.seek(0)
    panics = [l for l in log.read().splitlines() if "panicked at" in l]
    check("no panics", not panics, panics[0] if panics else "")

failed = [r for r in results if not r[1]]
print(f"\n{len(results) - len(failed)}/{len(results)} passed; app log: {log.name}")
sys.exit(1 if failed else 0)
