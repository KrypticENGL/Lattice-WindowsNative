# Lattice data-view helper, sourced into GDB by crates/debugger.
#
# Defines the MI command `-lattice-snapshot [MAX_OBJECTS]`. It walks the
# stopped thread's frames (outermost first), their arguments and locals,
# and everything reachable from them through pointers, references and
# by-value aggregates, and returns the result as one JSON string:
#
#   ^done,json="{...}"
#
# The schema is mirrored by crates/debugger/src/snapshot.rs. Objects are
# keyed by "address:type", so the same heap block keeps its id across stops.

import json
from collections import deque

import gdb

MAX_FRAMES = 12
MAX_ELEMENTS = 100
MAX_TEXT = 120

_POINTERS = (gdb.TYPE_CODE_PTR,)
_REFERENCES = (gdb.TYPE_CODE_REF, gdb.TYPE_CODE_RVALUE_REF)
_AGGREGATES = (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_UNION)


def _text(v, address=True):
    try:
        s = v.format_string(
            raw=False, pretty_structs=False, max_elements=8, repeat_threshold=10, address=address
        )
    except Exception:
        try:
            s = str(v)
        except Exception as e:  # noqa: BLE001 - any GDB failure becomes text
            s = "<%s>" % (str(e) or "unavailable")
    s = " ".join(s.split())
    return s if len(s) <= MAX_TEXT else s[: MAX_TEXT - 1] + "…"


def _type_name(t):
    try:
        return str(t)
    except Exception:
        return "?"


def _is_char(t):
    t = t.strip_typedefs()
    return t.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_CHAR) and t.sizeof == 1 and "char" in str(t)


class _Walker:
    def __init__(self, max_objects):
        self.max_objects = max_objects
        self.objects = {}
        self.queue = deque()
        self.truncated = False
        self.anon = 0

    # -- values ------------------------------------------------------------

    def value(self, v):
        """A value cell: scalar text, a pointer, or a link to an object."""
        try:
            t = v.type.strip_typedefs()
            code = t.code
            if code in _REFERENCES:
                return self.pointer_to(v.referenced_value(), _text(v.address) if v.address else "")
            if code in _POINTERS:
                return self.pointer(v, t)
            if code in _AGGREGATES or code == gdb.TYPE_CODE_ARRAY:
                pp = gdb.default_visualizer(v)
                if pp is not None and not hasattr(pp, "children"):
                    return {"k": "s", "v": self.pretty_text(pp)}
                if pp is not None and _hint(pp) == "string":
                    return {"k": "s", "v": self.pretty_text(pp)}
                if pp is not None and _hint(pp) is None:
                    # A smart pointer (unique_ptr, shared_ptr, ...) is drawn as
                    # the pointer it holds, not as a box around it.
                    inner = _single_pointer_child(pp)
                    if inner is not None:
                        return self.value(inner)
                if code == gdb.TYPE_CODE_ARRAY and _is_char(t.target()):
                    return {"k": "s", "v": _text(v)}
                link = self.intern(v)
                if link is None:
                    return {"k": "s", "v": _text(v)}
                return {"k": "o", "to": link}
            pp = gdb.default_visualizer(v)
            if pp is not None:
                return {"k": "s", "v": self.pretty_text(pp)}
            return {"k": "s", "v": _text(v)}
        except gdb.MemoryError:
            return {"k": "s", "v": "<unreadable>"}
        except gdb.error as e:
            return {"k": "s", "v": "<%s>" % (str(e) or "error")}

    def pointer(self, v, t):
        addr = int(v.cast(gdb.lookup_type("unsigned long long")))
        if addr == 0:
            return {"k": "p", "to": None, "v": "nullptr"}
        target = t.target().strip_typedefs()
        if target.code in (gdb.TYPE_CODE_FUNC, gdb.TYPE_CODE_VOID):
            return {"k": "s", "v": _text(v)}
        if _is_char(target):
            return {"k": "s", "v": _text(v, address=False)}
        hexaddr = "0x%x" % addr
        try:
            if target.code == gdb.TYPE_CODE_STRUCT:
                try:
                    v = v.cast(v.dynamic_type)
                except gdb.error:
                    pass
            obj = v.dereference()
            obj.fetch_lazy()
        except (gdb.MemoryError, gdb.error):
            return {"k": "p", "to": None, "v": hexaddr, "bad": True}
        return self.pointer_to(obj, hexaddr)

    def pointer_to(self, obj, hexaddr):
        link = self.intern(obj)
        if link is None:
            return {"k": "p", "to": None, "v": hexaddr, "cut": True}
        return {"k": "p", "to": link, "v": hexaddr}

    def pretty_text(self, pp):
        try:
            s = pp.to_string()
        except Exception as e:  # noqa: BLE001
            return "<%s>" % e
        if s is None:
            return ""
        if isinstance(s, gdb.Value):
            return _text(s)
        if hasattr(s, "value"):  # lazy string
            try:
                return _text(s.value())
            except Exception:
                return "<string>"
        s = " ".join(str(s).split())
        if _hint(pp) == "string" and not s.startswith('"'):
            s = '"%s"' % s
        return s if len(s) <= MAX_TEXT else s[: MAX_TEXT - 1] + "…"

    # -- objects -----------------------------------------------------------

    def intern(self, obj):
        """The id of the object holding `obj`, queueing it the first time."""
        try:
            addr = int(obj.address.cast(gdb.lookup_type("unsigned long long"))) if obj.address is not None else None
        except gdb.error:
            addr = None
        tname = _type_name(obj.type)
        if addr is None:
            self.anon += 1
            key = "v%d:%s" % (self.anon, tname)
        else:
            key = "%x:%s" % (addr, tname)
        if key in self.objects:
            return key
        if len(self.objects) >= self.max_objects:
            self.truncated = True
            return None
        self.objects[key] = None
        self.queue.append((key, obj, addr))
        return key

    def drain(self):
        while self.queue:
            key, obj, addr = self.queue.popleft()
            try:
                body = self.build(obj)
            except (gdb.MemoryError, gdb.error) as e:
                body = {"kind": "cell", "fields": [{"name": "", "type": "", "value": {"k": "s", "v": "<%s>" % e}}]}
            body["addr"] = None if addr is None else "0x%x" % addr
            body["type"] = _type_name(obj.type)
            try:
                body["size"] = obj.type.sizeof
            except gdb.error:
                body["size"] = 0
            self.objects[key] = body

    def build(self, obj):
        t = obj.type.strip_typedefs()
        pp = gdb.default_visualizer(obj)
        if pp is not None and hasattr(pp, "children"):
            return self.build_pretty(obj, pp)
        if t.code == gdb.TYPE_CODE_ARRAY:
            lo, hi = t.range()
            n = hi - lo + 1
            shown = min(n, MAX_ELEMENTS)
            elem = _type_name(t.target())
            fields = [{"name": str(lo + i), "type": elem, "value": self.value(obj[lo + i])} for i in range(shown)]
            return {"kind": "array", "fields": fields, "more": n - shown}
        if t.code in _AGGREGATES:
            fields = []
            self.struct_fields(obj, t, fields)
            return {"kind": "struct", "fields": fields}
        if pp is not None:
            return {"kind": "cell", "fields": [{"name": "", "type": _type_name(obj.type), "value": {"k": "s", "v": self.pretty_text(pp)}}]}
        return {"kind": "cell", "fields": [{"name": "", "type": _type_name(obj.type), "value": self.value(obj)}]}

    def struct_fields(self, obj, t, out):
        for f in t.fields():
            if not hasattr(f, "bitpos"):  # static member
                continue
            ft = f.type.strip_typedefs() if f.type is not None else None
            if f.is_base_class or (not f.name and ft is not None and ft.code in _AGGREGATES):
                try:
                    sub = obj[f]
                    self.struct_fields(sub, sub.type.strip_typedefs(), out)
                except gdb.error:
                    pass
                continue
            if f.name and f.name.startswith("_vptr"):
                continue
            try:
                cell = self.value(obj[f])
            except gdb.error as e:
                cell = {"k": "s", "v": "<%s>" % e}
            out.append({"name": f.name or "", "type": _type_name(f.type), "value": cell})

    def build_pretty(self, obj, pp):
        hint = _hint(pp)
        summary = self.pretty_text(pp)
        fields = []
        more = 0
        try:
            children = iter(pp.children())
        except Exception:  # noqa: BLE001
            children = iter(())
        if hint == "map":
            for i, (_, key) in enumerate(children):
                try:
                    _, val = next(children)
                except StopIteration:
                    break
                if i >= MAX_ELEMENTS:
                    more = -1
                    break
                kv = self.value(key) if isinstance(key, gdb.Value) else {"k": "s", "v": str(key)}
                name = kv["v"] if kv["k"] == "s" else _text(key)
                fields.append({"name": name, "type": "", "value": self._child(val)})
            return {"kind": "map", "fields": fields, "more": more, "summary": summary}
        for i, (name, val) in enumerate(children):
            if i >= MAX_ELEMENTS:
                more = -1
                break
            fields.append({"name": str(i) if hint == "array" else str(name), "type": "", "value": self._child(val)})
        kind = "array" if hint == "array" else "struct"
        return {"kind": kind, "fields": fields, "more": more, "summary": summary}

    def _child(self, val):
        if isinstance(val, gdb.Value):
            return self.value(val)
        return {"k": "s", "v": str(val)}


def _single_pointer_child(pp):
    try:
        it = iter(pp.children())
        first = next(it, None)
        if first is None or next(it, None) is not None:
            return None
        val = first[1]
        if isinstance(val, gdb.Value) and val.type.strip_typedefs().code in _POINTERS:
            return val
    except Exception:  # noqa: BLE001
        pass
    return None


def _hint(pp):
    try:
        return pp.display_hint() if hasattr(pp, "display_hint") else None
    except Exception:  # noqa: BLE001
        return None


def _frame_vars(frame, line, innermost):
    try:
        block = frame.block()
    except RuntimeError:
        return []
    args, locs, seen = [], [], set()
    while block is not None:
        for sym in block:
            if not (sym.is_argument or sym.is_variable) or sym.name in seen:
                continue
            # Compiler temporaries such as a range-for's __for_range.
            if getattr(sym, "is_artificial", False) or sym.name.startswith("__"):
                continue
            # Skip locals declared below the current line: still uninitialised.
            # In the innermost frame the current line has not run yet, so its
            # own declarations are skipped too; outer frames are mid-call on
            # their line, and what it declares (a range-for variable) is live.
            if not sym.is_argument and line and sym.line:
                if sym.line > line or (innermost and sym.line == line):
                    continue
            seen.add(sym.name)
            (args if sym.is_argument else locs).append(sym)
        if block.function is not None:
            break
        block = block.superblock
    locs.sort(key=lambda s: s.line or 0)
    return [(s, True) for s in args] + [(s, False) for s in locs]


def _has_user_frame(thread):
    thread.switch()
    f = gdb.newest_frame()
    while f is not None:
        if f.find_sal().symtab is not None and f.function() is not None:
            return True
        f = f.older()
    return False


def _select_user_thread():
    """After an interrupt, Windows GDB stops in a helper thread with no
    source. Switch to the first thread that has user code, so the snapshot
    and the next step use it."""
    current = gdb.selected_thread()
    if current is not None and _has_user_frame(current):
        return
    for t in sorted(gdb.selected_inferior().threads(), key=lambda t: t.num):
        if _has_user_frame(t):
            return
    if current is not None:
        current.switch()


def snapshot(max_objects):
    _select_user_thread()
    w = _Walker(max_objects)
    frames = []
    f = gdb.selected_frame()
    while f is not None and len(frames) < MAX_FRAMES:
        sal = f.find_sal()
        if sal.symtab is not None and f.function() is not None:
            frames.append(f)
        f = f.older()
    out = []
    for f in reversed(frames):
        sal = f.find_sal()
        vars_ = []
        for sym, is_arg in _frame_vars(f, sal.line, f == frames[0]):
            try:
                cell = w.value(sym.value(f))
            except (gdb.error, gdb.MemoryError) as e:
                cell = {"k": "s", "v": "<%s>" % e}
            vars_.append({"name": sym.name, "arg": is_arg, "type": _type_name(sym.type), "value": cell})
        out.append(
            {
                "level": f.level(),
                "func": f.name() or "??",
                "file": sal.symtab.fullname(),
                "line": sal.line,
                "vars": vars_,
            }
        )
    w.drain()
    return {"frames": out, "objects": w.objects, "truncated": w.truncated}


class _LatticeSnapshot(gdb.MICommand):
    def __init__(self):
        super().__init__("-lattice-snapshot")

    def invoke(self, argv):
        n = int(argv[0]) if argv else 2000
        return {"json": json.dumps(snapshot(n), separators=(",", ":"))}


_LatticeSnapshot()
