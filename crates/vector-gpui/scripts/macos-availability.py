#!/usr/bin/env python3
"""What a macOS build of the client reaches that is newer than its floor (MACOSX_DEPLOYMENT_TARGET in
.cargo/config.toml), judged by the SDK headers' availability macros: strongly linked frameworks and
C symbols (the app would not open), and Objective-C classes and selectors (a crash when called
unguarded). Only the libraries the binary loads, and what they re-export, are read. Classes and
selectors are matched by name in the binary, so each is a lead to check, with the source lines that
spell it.
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

imports = {}  # C symbol -> (weak?, library it binds from); a weakly loaded library makes its symbols weak
weak_libs = {os.path.basename(w).split(".")[0] for w in weak}
for l in subprocess.check_output(["nm", "-m", "-u", args.binary], text=True).splitlines():
    m = re.match(r"\s*\(undefined\) (weak )?external (\S+)(?: \(from (\S+)\))?", l)
    if m:
        imports[m.group(2).lstrip("_")] = (bool(m.group(1)) or m.group(3) in weak_libs, m.group(3))
class_imports = {s[len("OBJC_CLASS_$_"):] for s in imports if s.startswith("OBJC_CLASS_$_")}

root = os.path.join(sdk, "System", "Library", "Frameworks")

def reexports(fw):
    """Frameworks whose symbols `fw` re-exports, from its text stub in the SDK."""
    out = set()
    for dirpath, _, files in os.walk(os.path.join(root, fw + ".framework")):
        for f in files:
            if f == fw + ".tbd":
                text = open(os.path.join(dirpath, f), errors="replace").read()
                for section in re.findall(r"^reexported-libraries:\n((?:[ -].*\n)*)", text, re.M):
                    out |= set(re.findall(r"(\w+)\.framework", section))
    return out

def closure(names):
    seen, todo = set(), list(names)
    while todo:
        fw = todo.pop()
        if fw not in seen:
            seen.add(fw)
            todo += reexports(fw)
    return seen

loaded = {re.findall(r"(\w+)\.framework", p)[-1] for p in strong + weak if ".framework/" in p}
linked = closure(loaded)
lib_scope = {}
def scope_of(lib):
    if lib not in lib_scope:
        lib_scope[lib] = {"libSystem"} if lib and lib.startswith("libSystem") else closure({lib} if lib else set())
    return lib_scope[lib]

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
c_decls = defaultdict(list)        # imported C symbol -> [(version or None, framework)]
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
        name = declared_name(stmt)
        if name in imports:
            c_decls[name].append((version_in(stmt) or region, fw))

MACRO_CALL = re.compile(r"\b[A-Z_][A-Z0-9_]*\s*\(")

def declared_name(stmt):
    """The symbol a C declaration exports: none for inline bodies, types and macros."""
    if re.match(r"(typedef|struct|enum|union)\b", stmt) or re.search(r"\b(static|inline|__inline__?)\b", stmt):
        return None
    out, i = [], 0
    while (m := MACRO_CALL.search(stmt, i)):
        out.append(stmt[i:m.start()])
        depth, j = 0, m.end() - 1
        while j < len(stmt):
            depth += {"(": 1, ")": -1}.get(stmt[j], 0)
            if depth == 0:
                break
            j += 1
        i = j + 1
    s = "".join(out) + stmt[i:]
    m = re.search(r"\(\s*\*\s*(\w+)\s*\)", s) or re.search(r"([A-Za-z_]\w*)\s*\(", s)
    if m:
        return m.group(1)
    idents = re.findall(r"[A-Za-z_]\w*", s.split("=")[0].split("[")[0])
    return idents[-1] if idents else None

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

# Only what the binary loads can bind or answer at run time.
for dirpath, dirnames, files in os.walk(root):
    if os.path.basename(dirpath) != "Headers":
        continue
    fw = re.findall(r"(\w+)\.framework", dirpath)[-1]
    if fw not in linked:
        continue
    for f in files:
        if f.endswith(".h"):
            parse_header(os.path.join(dirpath, f), fw)
if any(os.path.basename(p).startswith("libSystem") for p in strong):
    for dirpath, dirnames, files in os.walk(os.path.join(sdk, "usr", "include")):
        for f in files:
            if f.endswith(".h"):
                parse_header(os.path.join(dirpath, f), "libSystem")

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

def leads(rust_name, call=False):
    # A selector is a method objc2 names after it: a call or a definition, not the bare word.
    pattern = ["-E", rf"(\.|fn ){rust_name}\(" if call else rf"\b{rust_name}\b"]
    out = subprocess.run(["grep", "-rn", "--include=*.rs"] + pattern + src_dirs,
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
# Newer only when every declaration in the library it binds from is: one without a macro is old.
for sym, (is_weak, lib) in sorted(imports.items()):
    vs = [v for v, fw in c_decls.get(sym, []) if fw in scope_of(lib)]
    if not is_weak and vs and all(newer(v) for v in vs):
        problems += 1
        print(f"  {sym} (macOS {fmt(min(vs))}, from {lib})")
for cls in sorted(class_imports):
    if newer(classes.get(cls)) and not imports["OBJC_CLASS_$_" + cls][0]:
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
    for l in leads(sel.rstrip(":").replace(":", "_"), call=True):
        print("      " + l)

print(f"\n{problems} to look at")
