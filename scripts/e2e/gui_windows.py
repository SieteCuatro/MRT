"""Several windows on the offscreen X server (:99): a tab running a command moves to a
new window with `:window new`, keeps running there, and comes back with `:window main`
typed into the new window. Screenshots of both windows go to the folder given as the
first argument. Start the X server first (scripts/e2e/xvfb_offscreen.sh)."""
import json, os, socket, struct, subprocess, sys, time, zlib, shutil

MRT = os.path.expanduser("~/mrt-target/debug/mrt")
R = os.path.expanduser("~/xvfb-local/root")
DATA = "/tmp/mrt-gui-windows"
SHOTS = sys.argv[1]
shutil.rmtree(DATA, ignore_errors=True)
os.makedirs(DATA)
env = dict(os.environ, MRT_DATA_DIR=DATA, DISPLAY=":99", LIBGL_ALWAYS_SOFTWARE="1", RUST_LOG="warn",
           LD_LIBRARY_PATH=f"{R}/usr/lib/x86_64-linux-gnu", XDG_RUNTIME_DIR=DATA + "-run")
env.pop("WAYLAND_DISPLAY", None)
os.makedirs(env["XDG_RUNTIME_DIR"], exist_ok=True)


def xdotool(*args):
    return subprocess.run([f"{R}/usr/bin/xdotool", *args], env=env, capture_output=True, text=True, timeout=20).stdout.strip()


def shot(name, window):
    xdotool("windowraise", window)
    time.sleep(1.0)
    xwd = subprocess.run([f"{R}/usr/bin/xwd", "-id", window, "-silent"], env=env, capture_output=True).stdout
    hs = struct.unpack(">I", xwd[0:4])[0]
    f = struct.unpack(">25I", xwd[0:100])
    w, h, bpl, nc = f[4], f[5], f[12], f[19]
    px = xwd[hs + nc * 12:]
    rows = []
    for y in range(h):
        row = px[y * bpl:y * bpl + w * 4]
        rgb = bytearray()
        for x in range(w):
            rgb += bytes((row[x * 4 + 2], row[x * 4 + 1], row[x * 4]))
        rows.append(b"\x00" + bytes(rgb))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)

    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"".join(rows), 6)) + chunk(b"IEND", b"")
    open(os.path.join(SHOTS, name), "wb").write(png)
    print("screenshot", name)


def call(tool, args):
    info = json.load(open(os.path.join(DATA, "mcp-bridge.json")))
    s = socket.create_connection(("127.0.0.1", info["port"]), timeout=60)
    req = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}
    s.sendall((json.dumps({"mrt_bridge_auth": info["token"]}) + "\n" + json.dumps(req) + "\n").encode())
    text = json.loads(s.makefile().readline())["result"]["content"][0]["text"]
    try:
        return json.loads(text)
    except Exception:
        return text


def wait(cond, what, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = cond()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.5)
    raise SystemExit("timed out: " + what)


results = []


def check(name, ok):
    results.append((name, bool(ok)))
    print(("PASS " if ok else "FAIL ") + name)


def windows_named(name):
    out = xdotool("search", "--onlyvisible", "--name", name)
    return [w for w in out.split() if w]


app = subprocess.Popen([MRT], env=env, stdout=open(DATA + ".log", "w"), stderr=subprocess.STDOUT, cwd="/tmp")
try:
    wait(lambda: os.path.exists(os.path.join(DATA, "mcp-bridge.json")) and call("mrt_list_tabs", {}), "window")
    time.sleep(3)
    created = call("mrt_create_tab", {"title": "worker", "switch_to": True})
    tab_id = created.get("tab_id") or created.get("id") if isinstance(created, dict) else None
    tabs = call("mrt_list_tabs", {})
    tab_list = tabs.get("tabs", tabs) if isinstance(tabs, dict) else tabs
    worker = next(t for t in tab_list if t.get("title") == "worker")
    tab_id = worker.get("tab_id") or worker.get("id")
    call("mrt_switch_tab", {"tab_id": tab_id})
    time.sleep(1)
    call("mrt_control_gui", {"input_text": "for i in $(seq 1 10); do echo tick-$i; sleep 1; done", "submit_input": True})
    time.sleep(2)
    call("mrt_control_gui", {"input_text": ":window new", "submit_input": True})
    second = wait(lambda: windows_named("window 2"), "second window", 20)
    check("a second window opened", second)
    main = [w for w in windows_named("Modern Rust Terminal") if w not in second]
    check("the main window is still there", main)
    xdotool("windowmove", second[0], "120", "60")
    time.sleep(2)
    shot("windows-second.png", second[0])
    shot("windows-main.png", main[0])
    raise_second = second[0]
    xdotool("windowraise", raise_second)

    def finished():
        blocks = call("mrt_list_blocks", {"tab_id": tab_id, "limit": 5})
        return [b for b in blocks if "tick-10" in (b.get("output") or "") and b.get("exit_code") == 0]

    check("the moved tab's command kept running and finished", wait(finished, "command in the moved tab", 40))
    shot("windows-second-done.png", second[0])

    # Type `:window main` into the second window.
    xdotool("windowfocus", "--sync", second[0])
    time.sleep(0.5)
    xdotool("type", "--delay", "40", ":window main")
    xdotool("key", "Return")
    check("the second window closed after :window main", wait(lambda: not windows_named("window 2"), "second window closes", 20))
    tabs = call("mrt_list_tabs", {})
    tab_list = tabs.get("tabs", tabs) if isinstance(tabs, dict) else tabs
    check("both tabs are in the main window again", len(tab_list) == 2)
    shot("windows-back.png", main[0])
finally:
    app.terminate()
    try:
        app.wait(10)
    except Exception:
        app.kill()
failed = [n for n, ok in results if not ok]
print(f"{len(results) - len(failed)} passed, {len(failed)} failed")
sys.exit(1 if failed else 0)
