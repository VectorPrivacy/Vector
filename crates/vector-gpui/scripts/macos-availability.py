#!/usr/bin/env python3
"""What a macOS build of the client reaches that is newer than its floor (MACOSX_DEPLOYMENT_TARGET in
.cargo/config.toml), judged by the SDK headers' availability macros: strongly linked frameworks and
C symbols (the app would not open), and Objective-C classes and selectors (a crash when called
unguarded). Classes and selectors are matched by name in the binary, so each is a lead to check,
with the source lines that spell it.
  scripts/macos-availability.py <binary> [--floor 12.0] [--deps] [--partial]
--deps also searches the dependencies' sources; --partial lists selectors only some of whose
declarations are newer than the floor."""
import argparse, json, os, re, subprocess, sys
from collections import defaultdict

here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
repo = os.path.dirname(os.path.dirname(here))

ap = argparse.ArgumentParser()
ap.add_argument("binary")
ap.add_argument("--floor")
ap.add_argument("--deps", action="store_true")
ap.add_argument("--partial", action="store_true")
args = ap.parse_args()

def floor_of_config():
    with open(os.path.join(here, ".cargo", "config.toml")) as f:
        return re.search(r'^MACOSX_DEPLOYMENT_TARGET = "(.*)"', f.read(), re.M).group(1)

def vt(s):
    return tuple(int(x) for x in re.split(r"[._]", s) if x) + (0,) * 3

floor = vt(args.floor or floor_of_config())[:3]
sdk = subprocess.check_output(["xcrun", "--sdk", "macosx", "--show-sdk-path"], text=True).strip()
blob = open(args.binary, "rb").read()

strong, weak = [], []
lines = subprocess.check_output(["otool", "-l", args.binary], text=True).splitlines()
for i, l in enumerate(lines):
    m = re.match(r"\s*cmd (LC_LOAD_DYLIB|LC_LOAD_WEAK_DYLIB)$", l)
    if m:
        name = re.match(r"\s*name (\S+)", lines[i + 2]).group(1)
        (weak if m.group(1).endswith("WEAK_DYLIB") else strong).append(name)

imports = {}  # C symbol -> weak? (a weakly loaded library makes every symbol from it weak)
weak_libs = {os.path.basename(w).split(".")[0] for w in weak}
for l in subprocess.check_output(["nm", "-m", "-u", args.binary], text=True).splitlines():
    m = re.match(r"\s*\(undefined\) (weak )?external (\S+)(?: \(from (\S+)\))?", l)
    if m:
        imports[m.group(2).lstrip("_")] = bool(m.group(1)) or m.group(3) in weak_libs
wanted = set(imports)
class_imports = {s[len("OBJC_CLASS_$_"):] for s in imports if s.startswith("OBJC_CLASS_$_")}

OLD = [
    r"NS_(?:CLASS_)?AVAILABLE(?:_MAC)?\(\s*(\d+_\d+)",
    r"NS_(?:CLASS_)?DEPRECATED(?:_MAC)?\(\s*(\d+_\d+)",
    r"__OSX_AVAILABLE_(?:STARTING|BUT_DEPRECATED)\(\s*__MAC_(\d+_\d+)",
    r"AVAILABLE_MAC_OS_X_VERSION_(\d+_\d+)_AND_LATER",
    r"CF_(?:AVAILABLE|DEPRECATED)(?:_MAC)?\(\s*(\d+_\d+)",
    r"__OSX_AVAILABLE\(\s*(\d+\.\d+)",
    r"CG_AVAILABLE_(?:STARTING|BUT_DEPRECATED)\(\s*(\d+\.\d+)",
]
OFF_MAC = re.compile(r"API_UNAVAILABLE\([^)]*\bmacos\b|__OSX_UNAVAILABLE|NS_UNAVAILABLE_MAC|__API_UNAVAILABLE\([^)]*\bmacos\b")
AVAIL = re.compile(r"\bmacosx?\s*\(\s*(\d+(?:[._]\d+){0,2})|" + "|".join(OLD))

def version_in(text):
    m = AVAIL.search(text)
    return vt(next(g for g in m.groups() if g))[:3] if m else None

def strip_parens(s):
    while True:
        t = re.sub(r"\([^()]*\)", " ", s)
        if t == s:
            return s
        s = t

classes = {}                       # class -> version
selectors = defaultdict(list)      # selector -> [(version or None, container)]
c_symbols = {}                     # wanted C symbol -> version
framework_min = {}                 # framework -> lowest annotated version

def record_statement(stmt, container, region, fw):
    if OFF_MAC.search(stmt) or (container and container[2] == "off"):
        return
    v = version_in(stmt) or (container and container[1]) or region
    if v:
        framework_min[fw] = min(framework_min.get(fw, v), v)
    name = container[0] if container else None
    if container and container[2] == "protocol" and stmt.startswith(("-", "+", "@property")):
        return
    if stmt.startswith(("-", "+")):
        body = strip_parens(stmt[1:])
        keys = re.findall(r"(\w+)\s*:", body)
        sel = "".join(k + ":" for k in keys) if keys else (re.match(r"\s*(\w+)", body) or [None, None])[1]
        if sel:
            selectors[sel].append((v, name))
    elif stmt.startswith("@property"):
        attrs = re.match(r"@property\s*\(([^)]*)\)", stmt)
        attrs = attrs.group(1) if attrs else ""
        idents = [w for w in re.findall(r"[A-Za-z_]\w*", strip_parens(stmt[9:])) if not re.fullmatch(r"[A-Z0-9_]+", w)]
        if not idents:
            return
        prop = idents[-1]
        getter = re.search(r"getter\s*=\s*(\w+)", attrs)
        selectors[getter.group(1) if getter else prop].append((v, name))
        if "readonly" not in attrs:
            setter = re.search(r"setter\s*=\s*(\w+:)", attrs)
            selectors[setter.group(1) if setter else "set" + prop[0].upper() + prop[1:] + ":"].append((v, name))
    else:
        for ident in set(re.findall(r"[A-Za-z_]\w*", stmt)) & wanted:
            if v and (ident not in c_symbols or v < c_symbols[ident]):
                c_symbols[ident] = v

def parse_header(path, fw):
    try:
        text = open(path, errors="replace").read()
    except OSError:
        return
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", "", text)
    container, regions, pending = None, [], ""
    for raw in text.splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        m = re.match(r"API_AVAILABLE_BEGIN\s*\((.*)\)", s)
        if m:
            regions.append(version_in(m.group(1)))
            continue
        if s.startswith("API_AVAILABLE_END"):
            regions and regions.pop()
            continue
        region = next((r for r in reversed(regions) if r), None)
        if s.startswith("@end"):
            container, pending = None, ""
            continue
        m = re.match(r"@(interface|protocol)\s+(\w+)(\s*\()?", s)
        if m and not s.endswith(";"):
            v = version_in(pending + " " + s) or region
            kind = "off" if OFF_MAC.search(pending + " " + s) else m.group(1)
            container = (m.group(2), v, kind)
            if kind == "interface" and not m.group(3):
                classes[m.group(2)] = v
            pending = ""
            continue
        # A method or property starts its own statement; its availability follows it.
        pending = s if s.startswith(("-", "+", "@property")) else pending + " " + s
        if pending.rstrip().endswith((";", "}")):
            record_statement(pending.strip(), container, region, fw)
            pending = ""

root = os.path.join(sdk, "System", "Library", "Frameworks")
for dirpath, dirnames, files in os.walk(root):
    if os.path.basename(dirpath) != "Headers":
        continue
    fw = re.findall(r"(\w+)\.framework", dirpath)[-1]
    for f in files:
        if f.endswith(".h"):
            parse_header(os.path.join(dirpath, f), fw)

def newer(v):
    return v is not None and v > floor

def fmt(v):
    return ".".join(map(str, v[:2] if v[2] == 0 else v))

src_dirs = [os.path.join(here, d) for d in ("src", "media/src", "vendor")] + [os.path.join(repo, "src-tauri", "src")]
if args.deps:
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", "x86_64-apple-darwin"], cwd=here))
    skip = re.compile(r"^(objc2|block2|dispatch2|objc-sys|core-foundation|core-graphics|core-text|core-video|cocoa|icrate|coreaudio-sys|security-framework-sys|io-kit-sys)")
    src_dirs += [os.path.dirname(p["manifest_path"]) for p in meta["packages"]
                 if p["source"] and not skip.match(p["name"])]

def leads(rust_name):
    out = subprocess.run(["grep", "-rnw", "--include=*.rs", "-e", rust_name] + src_dirs,
                         capture_output=True, text=True).stdout.splitlines()
    return [l.replace(repo + "/", "")[:160] for l in out if "/target/" not in l][:4]

problems = 0
print(f"floor macOS {fmt(floor)}, SDK {os.path.basename(sdk)}")
print("\n== Frameworks linked strongly whose API is all newer than the floor (the app will not open)")
for path in strong:
    fw = re.findall(r"(\w+)\.framework", path)
    if fw and newer(framework_min.get(fw[-1])):
        problems += 1
        print(f"  {fw[-1]} (from {fmt(framework_min[fw[-1]])})")
print("  weak: " + ", ".join(re.findall(r"(\w+)\.framework", " ".join(weak))) if weak else "  weak: none")

print("\n== C symbols bound strongly that are newer than the floor (the app will not open)")
for sym, v in sorted(c_symbols.items()):
    if newer(v) and not imports.get(sym):
        problems += 1
        print(f"  {sym} (macOS {fmt(v)})")
for cls in sorted(class_imports):
    if newer(classes.get(cls)) and not imports.get("OBJC_CLASS_$_" + cls):
        problems += 1
        print(f"  class {cls} (macOS {fmt(classes[cls])})")

print("\n== Classes newer than the floor that the binary names")
for cls, v in sorted(classes.items()):
    if newer(v) and (cls.encode() + b"\0") in blob:
        problems += 1
        print(f"  {cls} (macOS {fmt(v)})")
        for l in leads(cls):
            print("      " + l)

print("\n== Selectors newer than the floor that the binary names")
for sel, decls in sorted(selectors.items()):
    vs = [d[0] for d in decls]
    if not any(newer(v) for v in vs) or (sel.encode() + b"\0") not in blob:
        continue
    every = all(newer(v) for v in vs)
    if not every and not args.partial:
        continue
    problems += every
    owners = sorted({f"{d[1]} {fmt(d[0])}" for d in decls if newer(d[0])})[:4]
    print(f"  {sel}{'' if every else ' (some declarations)'}: {', '.join(owners)}")
    for l in leads(sel.rstrip(":").replace(":", "_")):
        print("      " + l)

print(f"\n{problems} to look at")
