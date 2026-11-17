"""Feature tour of the real MRT window on the offscreen X server (:99): runs features
through MCP, checks results over MCP, and takes screenshots."""
import json, os, socket, struct, subprocess, sys, time, zlib, shutil, base64

MRT = os.path.expanduser("~/mrt-target/debug/mrt")
R = os.path.expanduser("~/xvfb-local/root")
DATA = "/tmp/mrt-gui-tour"
SHOTS = sys.argv[1]
shutil.rmtree(DATA, ignore_errors=True)
os.makedirs(DATA)
cfg = os.path.join(DATA, "config.toml")
open(cfg, "w").write('[terminal]\ntheme = "high-contrast"\n\n[[actions]]\nname = "hello"\ncommand = "echo named-action-ran"\n')
env = dict(os.environ, MRT_DATA_DIR=DATA, MRT_CONFIG=cfg, DISPLAY=":99", LIBGL_ALWAYS_SOFTWARE="1", RUST_LOG="warn",
           LD_LIBRARY_PATH=f"{R}/usr/lib/x86_64-linux-gnu", XDG_RUNTIME_DIR="/tmp/mrt-gui-tour-run")
env.pop("WAYLAND_DISPLAY", None)
os.makedirs(env["XDG_RUNTIME_DIR"], exist_ok=True)

def shot(name):
    xwd = subprocess.run([f"{R}/usr/bin/xwd", "-root", "-silent"], env=env, capture_output=True).stdout
    hs = struct.unpack(">I", xwd[0:4])[0]
    f = struct.unpack(">25I", xwd[0:100])
    w, h, bpl, nc = f[4], f[5], f[12], f[19]
    px = xwd[hs + nc * 12:]
    rows = []
    for y in range(h):
        row = px[y * bpl:y * bpl + w * 4]
        rgb = bytearray()
        for x in range(w):
            rgb += bytes((row[x*4+2], row[x*4+1], row[x*4]))
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

def type_cmd(text):
    call("mrt_control_gui", {"input_text": text, "submit_input": True})

def block_with(text, limit=30):
    blocks = call("mrt_list_blocks", {"tab_id": "", "limit": limit})
    return [b for b in blocks if text in (b.get("output") or "") or text in b.get("command", "")]

results = []
def check(name, ok):
    results.append((name, bool(ok)))
    print(("PASS " if ok else "FAIL ") + name)

win = subprocess.Popen([MRT], env=env, stdout=open("/tmp/mrt-tour.log", "w"), stderr=subprocess.STDOUT, cwd="/tmp")
try:
    wait(lambda: os.path.exists(os.path.join(DATA, "mcp-bridge.json")) and call("mrt_list_tabs", {}), "window")
    time.sleep(3)

    # Commands through the session host, streaming output, exit codes.
    type_cmd("echo tour-host-ok && false")
    check("command block with exit code (session host)", wait(lambda: [b for b in block_with("tour-host-ok") if b["exit_code"] == 1], "exit block"))
    # Inline image (iTerm2 protocol) in a block: a 2x2 PNG.
    def mkpng(w, h):
        raw = b''.join(bytes([0]) + b''.join(bytes((x * 255 // w, y * 255 // h, 180)) for x in range(w)) for y in range(h))
        def chunk(t, d):
            return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
        return bytes([137, 80, 78, 71, 13, 10, 26, 10]) + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')
    png = mkpng(64, 48)
    open("/tmp/tour.png", "wb").write(png)
    type_cmd("printf 'img:\\033]1337;File=inline=1;width=20:%s\\a\\nafter-image\\n' \"$(base64 -w0 /tmp/tour.png)\"")
    check("inline image command ran", wait(lambda: block_with("after-image"), "image block"))
    # Named action from the config.
    type_cmd(":assist")
    check(":assist investigation block", wait(lambda: [b for b in call("mrt_list_blocks", {"tab_id": "", "limit": 30}) if b["command"] == ":assist" and b["exit_code"] is not None], "assist"))
    shot("tour-1-blocks-image-assist.png")

    type_cmd(":ps --limit 5")
    check(":ps process table", wait(lambda: [b for b in call("mrt_list_blocks", {"tab_id": "", "limit": 30}) if b["command"].startswith(":ps") and b["exit_code"] == 0], "ps"))
    type_cmd(":services cron")
    check(":services table", wait(lambda: [b for b in call("mrt_list_blocks", {"tab_id": "", "limit": 30}) if b["command"].startswith(":services") and b["exit_code"] is not None], "services"))
    type_cmd(":sandbox /tmp")
    check(":sandbox tab", wait(lambda: [t for t in call("mrt_list_tabs", {}) if t.get("sandbox")], "sandbox tab"))
    type_cmd("rm -rf /tmp/nothing-here-mrt")
    time.sleep(1.5)
    state = call("mrt_get_session_state", {})
    check("sandbox asks before a non-safe command (Safety Gate prompt)", state.get("pending_approval"))
    shot("tour-2-sandbox-safety-gate.png")
    call("mrt_control_gui", {"input_text": "", "submit_input": False})
    # Deny via the modal is not reachable from MCP without remote approvals; quit the prompt by quitting later.

    print("\n".join(f"{'PASS' if ok else 'FAIL'}  {n}" for n, ok in results))
    print("TOUR", "OK" if all(ok for _, ok in results) else "HAD FAILURES")
finally:
    try:
        call("mrt_control_gui", {"window": "quit"})
    except Exception:
        pass
    time.sleep(2)
    win.kill()
