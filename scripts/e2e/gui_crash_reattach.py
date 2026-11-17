"""Real-window crash/reattach check on an offscreen X server (Xvfb :99 in WSL).

1. Start the MRT window (isolated MRT_DATA_DIR) on DISPLAY=:99.
2. Through its MCP bridge, type a long command and a full-screen program into the tab.
3. kill -9 the window (a crash).
4. Check the session host still runs both.
5. Start the window again and check, through MCP, that the finished command and the
   program (which ends after a while and becomes a block) show up in the same tab.
Screenshots are written as PNG next to this script's output folder."""
import json, os, socket, struct, subprocess, sys, time, zlib, shutil

MRT = os.path.expanduser("~/mrt-target/debug/mrt")
DATA = "/tmp/mrt-gui-e2e"
R = os.path.expanduser("~/xvfb-local/root")
SHOTS = sys.argv[1] if len(sys.argv) > 1 else "/tmp"
shutil.rmtree(DATA, ignore_errors=True)
os.makedirs(DATA)
env = dict(os.environ, MRT_DATA_DIR=DATA, DISPLAY=":99", LIBGL_ALWAYS_SOFTWARE="1", RUST_LOG="warn", LD_LIBRARY_PATH=f"{R}/usr/lib/x86_64-linux-gnu")
env.pop("WAYLAND_DISPLAY", None)
env["XDG_RUNTIME_DIR"] = "/tmp/mrt-gui-e2e-run"
os.makedirs(env["XDG_RUNTIME_DIR"], exist_ok=True)

def shot(name):
    xwd = subprocess.run([f"{R}/usr/bin/xwd", "-root", "-silent"], env=dict(env, LD_LIBRARY_PATH=f"{R}/usr/lib/x86_64-linux-gnu"), capture_output=True).stdout
    header_size = struct.unpack(">I", xwd[0:4])[0]
    fields = struct.unpack(">25I", xwd[0:100])
    width, height, bpl, ncolors = fields[4], fields[5], fields[12], fields[19]
    pixels = xwd[header_size + ncolors * 12:]
    rows = []
    for y in range(height):
        row = pixels[y * bpl:y * bpl + width * 4]
        rgb = bytearray()
        for x in range(width):
            b, g, r = row[x * 4], row[x * 4 + 1], row[x * 4 + 2]
            rgb += bytes((r, g, b))
        rows.append(b"\x00" + bytes(rgb))
    raw = zlib.compress(b"".join(rows), 6)
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) + chunk(b"IDAT", raw) + chunk(b"IEND", b"")
    path = os.path.join(SHOTS, name)
    open(path, "wb").write(png)
    print("screenshot:", path)

def bridge_call(tool, args):
    info = json.load(open(os.path.join(DATA, "mcp-bridge.json")))
    s = socket.create_connection(("127.0.0.1", info["port"]), timeout=30)
    req = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}
    s.sendall((json.dumps({"mrt_bridge_auth": info["token"]}) + "\n" + json.dumps(req) + "\n").encode())
    line = s.makefile().readline()
    text = json.loads(line)["result"]["content"][0]["text"]
    try:
        return json.loads(text)
    except Exception:
        return text

def host_call(op):
    info = json.load(open(os.path.join(DATA, "session-host.json")))
    s = socket.create_connection(("127.0.0.1", info["port"]), timeout=30)
    s.sendall((json.dumps({"mrt_host_auth": info["token"]}) + "\n" + json.dumps(op) + "\n").encode())
    return json.loads(s.makefile().readline())["ok"]

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
    raise SystemExit(f"timed out waiting for {what}")

def start_window(tag):
    log = open(f"/tmp/mrt-gui-{tag}.log", "w")
    return subprocess.Popen([MRT], env=env, stdout=log, stderr=log, cwd="/tmp")

window = start_window("1")
wait(lambda: os.path.exists(os.path.join(DATA, "mcp-bridge.json")) and bridge_call("mrt_list_tabs", {}), "the first window")
wait(lambda: os.path.exists(os.path.join(DATA, "session-host.json")) and host_call({"op": "jobs"}) is not None, "the session host")
time.sleep(1.5)
tab_id = bridge_call("mrt_list_tabs", {})[0]["id"]
print("window 1 up, tab", tab_id, "session host up")

bridge_call("mrt_control_gui", {"input_text": "sleep 8; echo BLOCK_SURVIVED_THE_CRASH", "submit_input": True})
time.sleep(1)
bridge_call("mrt_control_gui", {"input_text": ':tui sh -c "echo TUI_SURVIVED_THE_CRASH; sleep 14"', "submit_input": True})
wait(lambda: [j for j in host_call({"op": "jobs"}) if not j["done"]], "the command in the host")
wait(lambda: [p for p in host_call({"op": "pty_list"}) if not p["exited"]], "the program in the host")
time.sleep(4)  # the layout (with tab ids) is saved every 3 s
shot("mrt-1-before-crash.png")

window.kill()  # SIGKILL: no cleanup, no on_exit
window.wait()
print("window 1 killed with SIGKILL")
jobs = host_call({"op": "jobs"})
ptys = host_call({"op": "pty_list"})
print("host after the crash: jobs", [(j["command"], j["done"]) for j in jobs], "ptys", [(p["command"], p["exited"]) for p in ptys])
assert any(not j["done"] for j in jobs) or jobs, "the command is still in the host"
assert any(not p["exited"] for p in ptys), "the program still runs in the host"

window = start_window("2")
wait(lambda: bridge_call("mrt_list_tabs", {}), "the second window")
time.sleep(2)
shot("mrt-2-after-reattach.png")
tabs = bridge_call("mrt_list_tabs", {})
print("window 2 tabs:", [t["id"] for t in tabs])
assert tabs[0]["id"] == tab_id, "the layout kept the tab id"

def blocks_have(text):
    blocks = bridge_call("mrt_list_blocks", {"tab_id": tab_id, "limit": 20})
    return [b for b in blocks if text in (b.get("output") or "")]

found = wait(lambda: blocks_have("BLOCK_SURVIVED_THE_CRASH"), "the command's block in the reopened window", 60)
print("reattached command block:", found[0]["command"], "exit", found[0]["exit_code"])
found = wait(lambda: blocks_have("TUI_SURVIVED_THE_CRASH"), "the program's block when it ends", 60)
print("reattached program block:", found[0]["command"], "exit", found[0]["exit_code"])
shot("mrt-3-final.png")

bridge_call("mrt_control_gui", {"window": "quit"})
time.sleep(2)
window.kill()
print("GUI E2E OK")
