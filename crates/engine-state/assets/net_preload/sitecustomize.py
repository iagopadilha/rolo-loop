"""Python half of the loop's run-time egress layer (RUN-02). Standard library only, no Rolo Loop imports: it runs inside ANY Python process an agent
command starts (the guard puts this folder first on PYTHONPATH, so `site` imports this file as `sitecustomize` at interpreter start-up).

It does nothing unless LOOP_NETWORK_ALLOW_FILE names the project's `network-allow.json` (written by the engine at every turn start). With it,
`socket.socket.connect` / `connect_ex`, `socket.create_connection`, `socket.getaddrinfo` and `socket.gethostbyname[_ex]` refuse a host that the
list does not allow, BEFORE any DNS or TCP, with
    ConnectionRefusedError("[egress-runtime] <host>:<port> not in the project's network list")
Host-level check only (HTTPS hides path and method from anything but the process itself): a host passes when
  * it is local (localhost, 127.0.0.1, ::1, 0.0.0.0, *.localhost) and not denied, or
  * an HTTP entry matches the host and the port (the entry's port, or 80/443 when the entry has none), or
  * a `tcp` entry matches host:port (the only way to reach a non-HTTP port, RUN-04).
A deny pattern wins over everything; an unreadable or missing list fails closed for every non-local host. A connect to an IP address passes
when the IP is local, was returned by an allowed lookup in this process, or is itself listed as a host.
Residual: `_socket` used directly, other runtimes (.NET, Java, Go...) and `python -S|-E|-I` skip it (the guard blocks those flags).
Any other `sitecustomize` later on sys.path still runs (chained at the end).
"""
import os
import sys


def _install():
    allow_file = os.environ.get("LOOP_NETWORK_ALLOW_FILE")
    if not allow_file:
        return
    import json
    import socket
    import time

    local_names = ("localhost", "127.0.0.1", "::1", "0.0.0.0", "")
    http_ports = (80, 443)
    inet_families = (socket.AF_INET, socket.AF_INET6)
    state = {"stamp": None, "list": None}
    resolved = {}  # ip -> {host names an allowed lookup of this process returned it for}

    def read_list():
        try:
            info = os.stat(allow_file)
            stamp = (info.st_mtime_ns, info.st_size)
            if stamp == state["stamp"]:
                return state["list"]
            with open(allow_file, encoding="utf-8-sig") as handle:
                content = json.load(handle)
            if not isinstance(content, dict) or not isinstance(content.get("allow"), list):
                raise ValueError("shape")
            state["stamp"], state["list"] = stamp, content
        except (OSError, ValueError):
            state["stamp"], state["list"] = None, None
        return state["list"]

    def clean(host):
        if isinstance(host, bytes):
            host = host.decode("ascii", "replace")
        text = str(host or "").strip().lower()
        if text.startswith("[") and text.endswith("]"):
            text = text[1:-1]
        text = text.split("%", 1)[0] if ":" in text else text  # IPv6 scope id
        return text[:-1] if text.endswith(".") and not text.endswith("..") else text

    def host_matches(pattern, host):
        if pattern.startswith("*."):
            suffix = pattern[1:]
            return host.endswith(suffix) and len(host) > len(suffix) and all(host[:-len(suffix)].split("."))
        return pattern == host

    def is_local(host):
        return host in local_names or host.endswith(".localhost")

    def is_ip(host):
        for family in inet_families:
            try:
                socket.inet_pton(family, host)
                return True
            except (OSError, ValueError):
                pass
        return False

    def to_port(port):
        try:
            return int(port)
        except (TypeError, ValueError):
            return None

    def allowed(host, port):
        """True when the connection to host:port may happen. `port` None = a lookup (the port is judged at connect time)."""
        content = read_list()
        denies = content.get("deny") if content else None
        for pattern in denies if isinstance(denies, list) else ():
            if isinstance(pattern, str) and host_matches(pattern, host):
                return False
        if is_local(host):
            return True
        if not content or content.get("broken"):
            return False
        defined = content.get("env_names") or ()
        for entry in content["allow"]:
            if not isinstance(entry, dict) or entry.get("origin") == "local":
                continue
            if entry.get("if_missing") and entry["if_missing"] in defined:
                continue
            if not host_matches(str(entry.get("host", "")), host):
                continue
            if port is None:
                return True
            entry_port = entry.get("port")
            if "tcp" in (entry.get("methods") or ()):
                if entry_port is None or entry_port == port:
                    return True
            elif entry_port == port or (entry_port is None and port in http_ports):
                return True
        return False

    def note_denial(host, port):
        try:
            runtime = os.path.join(os.path.dirname(os.path.abspath(allow_file)), "network-runtime.json")
            with open(runtime, encoding="utf-8-sig") as handle:
                info = json.load(handle)
            target = info.get("egress_log")
            if not target:
                return
            line = {"time": time.strftime("%Y-%m-%dT%H:%M:%S"), "project": info.get("project", ""), "turn": info.get("turn"), "layer": "python-preload",
                    "host": host, "port": port, "reason": "not in the project's network list"}
            with open(target, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(line) + "\n")
        except Exception:  # noqa: BLE001  (the log is best effort: the refusal itself never depends on it)
            pass

    def refuse(host, port):
        note_denial(host, port)
        where = host if port is None else "%s:%s" % (host, port)
        raise ConnectionRefusedError("[egress-runtime] %s not in the project's network list" % where)

    def check_name(host, port=None):
        """Lookup or connect by name or IP literal."""
        host = clean(host)
        if is_ip(host) and port is not None:
            if is_local(host) and allowed(host, port):
                return
            names = resolved.get(host, ())
            if any(allowed(name, port) for name in names) or allowed(host, port):
                return
            refuse(host, port)
        if not allowed(host, port):
            refuse(host, port)

    def check_address(address):
        if isinstance(address, tuple) and len(address) >= 2:
            check_name(address[0], to_port(address[1]))

    original_connect = socket.socket.connect
    original_connect_ex = socket.socket.connect_ex
    original_create_connection = socket.create_connection
    original_getaddrinfo = socket.getaddrinfo
    original_gethostbyname = socket.gethostbyname
    original_gethostbyname_ex = socket.gethostbyname_ex

    def connect(self, address):
        if self.family in inet_families:
            check_address(address)
        return original_connect(self, address)

    def connect_ex(self, address):
        if self.family in inet_families:
            check_address(address)
        return original_connect_ex(self, address)

    def create_connection(address, *args, **kwargs):
        check_address(address)
        return original_create_connection(address, *args, **kwargs)

    def getaddrinfo(host, port, *args, **kwargs):
        if host is None or host == "":
            return original_getaddrinfo(host, port, *args, **kwargs)
        name = clean(host)
        if not is_ip(name):
            check_name(name)
        result = original_getaddrinfo(host, port, *args, **kwargs)
        if not is_ip(name):
            for item in result:
                if item[0] in inet_families:
                    resolved.setdefault(item[4][0], set()).add(name)
        return result

    def gethostbyname(host):
        if not is_ip(clean(host)):
            check_name(host)
        return original_gethostbyname(host)

    def gethostbyname_ex(host):
        if not is_ip(clean(host)):
            check_name(host)
        return original_gethostbyname_ex(host)

    socket.socket.connect = connect
    socket.socket.connect_ex = connect_ex
    socket.create_connection = create_connection
    socket.getaddrinfo = getaddrinfo
    socket.gethostbyname = gethostbyname
    socket.gethostbyname_ex = gethostbyname_ex


def _chain():
    """Runs the next `sitecustomize` on sys.path (the one this file shadows), if there is one."""
    import importlib.machinery
    import importlib.util
    here = os.path.normcase(os.path.dirname(os.path.abspath(__file__)))
    others = [path for path in sys.path if os.path.normcase(os.path.abspath(path or os.getcwd())) != here]
    spec = importlib.machinery.PathFinder.find_spec("sitecustomize", others)
    if spec is not None and spec.loader is not None:
        spec.loader.exec_module(importlib.util.module_from_spec(spec))


try:
    _install()
except Exception as _error:  # noqa: BLE001  (a broken preload must not break the interpreter; the guard's static layer still applies)
    sys.stderr.write("[egress-runtime] python preload not installed: %s: %s\n" % (type(_error).__name__, _error))
_chain()
