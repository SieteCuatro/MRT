# Session Server and Remote Sessions — Design

This page covers what is built, and the plan for the rest of Milestone 4 in the [ROADMAP](../ROADMAP.md#milestone-4--sessions-that-outlive-the-window): a server that owns the shells, a GUI that attaches to it, surviving upgrades, and attaching from another computer.

## 1. What exists today

| Piece | Where | What it does |
| :--- | :--- | :--- |
| Bridge | `mrt-mcp/src/bridge.rs` | Localhost TCP, one random 64-hex token per run, written with the port and PID to `mcp-bridge.json` (`0600` on Unix). One JSON-RPC request per line after an auth line. |
| `mrt server` | `mrt-app/src/server.rs` | A headless process holding an `MrtMcpServerState`: tabs, command blocks and interactive PTYs. It serves them on the bridge until Ctrl+C. It refuses to start when a window or another server already answers. |
| Clients | `mrt list/send/capture/split`, `mrt --mcp-server`, agents | They speak MCP `tools/call` to whichever process owns the discovery file. |
| Detach for PTYs | `mrt_pty_spawn` / `mrt_pty_write` / `mrt_pty_read` | PTYs live in the server, not the client. `mrt_pty_read` with `drain: true` returns only the output a client has not read yet, so a client that reconnects gets what it missed once. `view: "screen"` rebuilds the visible screen. |
| Layout restore | `mrt-app/src/layout.rs` | The GUI saves its tabs, splits, shells, folders and remote tabs, and reopens them with fresh shells. |

| Session host | `mrt-app/src/session_host.rs` | `mrt session-host`, started detached by the window: owns each tab's persistent shell (`ShellPool`) and every running command with its output. The window sends `run`, follows with `poll`, and on start asks for `jobs` to reattach to commands that ran while it was gone. Token-authenticated localhost TCP (`session-host.json`). |
| Sessions outlive the window | `mrt-app/src/app.rs` (`handle_close_request`) | Closing the window while commands, TUI programs or PTYs run keeps the MRT process running with its window hidden; `mrt` / `mrt quick` shows it again with the same tabs, blocks and live output. |
| Single instance and upgrades | `mrt-app/src/cli.rs` (`handoff_to_running_instance`) | A new `mrt` shows the running window. A different version replaces an idle one (which saves its layout) and leaves a busy one running. |
| Remote connections | `mrt-mcp/src/remote.rs`, `mrt-app/src/remote_cli.rs` | `mrt share` / `mrt pair` / `mrt attach`: SPAKE2 pairing, Noise-encrypted relay to the bridge, paired keys only, live revocation, reconnect with missed output shown once. |
| Phone companion | `mrt-mcp/src/companion.rs` | `mrt share --web`: HTTPS web app with token pairing, live tabs and blocks, a command box and Safety Gate answers (opt-in). |

With the session host, the shells, running commands, full-screen programs and agents' PTYs no longer live in the window process: a window that closes or crashes does not take them along, and the next window reattaches.

**Event streams.** Both kinds of work are followed over long-lived connections on which the host pushes events, not by polling:

- `job_stream` (command blocks): `{"out": text}` lines as output appears, then `{"done": {exit_code, duration_ms, cwd}, "output": full}`. It starts at a byte offset, so a dropped stream resumes where it stopped.
- `pty_stream` (programs, `mrt_core::pty_host`): `{"ok": {pid, cols, rows}}`, then `{"out"}` / `{"exit"}` lines from the host; `{"in": base64}`, `{"resize": [c, r]}`, `{"kill": true}` from the window. Starting at offset 0 replays the whole session, which rebuilds the screen of a reattached program.

`InteractivePty` has a hosted backend with the same API, so the TUI overlay, pane programs and `mrt_pty_*` do not know whether their PTY is local or in the host. PTYs carry an owner label (`{"kind": "tui", "tab_id", "pane", "command"}` or `{"kind": "mcp"}`) that tells a new window where to put them back.

## 2. GUI attached to the server (implemented)

**Goal.** Close the window during a build, reopen it, and see the same tabs, the finished blocks and the live output.

1. **One owner.** `mrt` (GUI) first looks for a server through `mcp-bridge.json` + `ping`. When none answers, it starts `mrt server --detached` as a child that outlives it (on Windows `DETACHED_PROCESS | CREATE_NO_WINDOW`; on Unix `setsid`), waits for the discovery file, and attaches.
2. **State moves into the server.** `ShellPool`, the TUI and pane PTYs, running blocks and the activity trackers move from `MrtGuiApp` into the server crate. The GUI keeps only view state: scroll, find, selection, the panels, and drafts.
3. **Protocol.** Requests stay MCP `tools/call`, so agents and the CLI need no change. A new long-lived **event stream** on the same connection replaces the per-frame shared buffers:
   - `notifications/mrt/block` (started, output chunk with a sequence number, completed);
   - `notifications/mrt/pty` (bytes with a sequence number);
   - `notifications/mrt/tabs` (tab list and activity changed).

   On attach, the GUI sends the last sequence number it saw for each stream. The server replays from there, keeping a bounded ring per block and PTY, capped like `terminal.max_block_output_mb`. Then it streams live.
4. **Input.** Typed commands go through `mrt_run_command`, extended with `origin: "gui"` so the Safety Gate approval runs in the attached window. Keys for PTYs go through `mrt_pty_write` with raw bytes.
5. **Several windows.** Each client gets the same events. Approvals go to the window that sent the command.
6. **Layout restore after a server restart.** The server writes `layout.json`, not the GUI, and recreates tabs with fresh shells when it starts.

## 3. Surviving upgrades

Two options, in order of preference:

- **Keep the old server running.** A new MRT version starts its own server only when the old one has no running commands or PTYs. Until then, the old server keeps serving and the new GUI attaches to it. Both versions must speak the protocol of the older one, so the protocol is versioned through MCP `initialize` (`protocolVersion` plus an `mrt` capability with a version number).
- **Hand over sessions.** The old server passes its PTY master handles to the new one: on Unix with `SCM_RIGHTS` over a Unix socket, on Windows with `DuplicateHandle` into the new process (ConPTY handles can be duplicated). It also passes the block rings and the `ShellSession` state (integration markers, cwd). This is more complex, so it is the second step.

## 4. Remote connections between MRT instances (implemented)

### Local network

- **Pairing.** The host runs `mrt share` (or uses a Share button), which shows a 6-digit code and a QR code containing its address and the fingerprint of its key. The guest runs `mrt attach <host>` and types the code. Both sides derive a shared secret with SPAKE2 from the code, then exchange long-term Ed25519 public keys under it. The host shows "Allow *laptop* to attach to *tab 2*?" and stores the guest's key only after the user approves.
- **Transport.** Noise `XX` (or TLS 1.3 with both certificates pinned to the paired keys) over TCP on a port the user chooses. There is no trust in network identity and no unencrypted mode.
- **Scope.** A pairing grants specific tabs, or all tabs. The host can revoke it from a list of paired devices, which drops live connections at once. Guests see the host name, shell and folder on each attached tab.
- **Same protocol.** Over the encrypted channel the guest speaks the GUI protocol from section 2, with the same tools and event streams. Commands from a guest always go through the host's Safety Gate, shown on the host. A tab can also be marked as approved-by-guest.

### Reconnect milestone

The test for this milestone: start a build on the host, attach from the laptop, and disconnect the laptop mid-build. When the laptop reconnects to the same tab, the build has continued, and the laptop shows the missed output exactly once, including the exit status if the build finished.

This works because of the sequence numbers in section 2. The guest resends its last sequence numbers on reconnect, and the server replays from its rings. If the gap is larger than a ring, the server sends a "truncated N bytes" marker followed by the block's completed state.

### Over the internet

After the LAN milestone, there are three candidate transports:

1. A relay, which only forwards bytes that are already Noise-encrypted.
2. QUIC with NAT hole punching.
3. Documented use of an existing overlay network (Tailscale, WireGuard), with LAN pairing on top.

The pairing and the encryption do not change. Only the way the two ends reach each other does.

## 5. Mobile companion (implemented as a web app)

A phone app is a paired guest from section 4 with a smaller UI: a tab list with activity, a block list, follow-output, and approve or deny for Safety Gate prompts and agents waiting for approval. It needs the session server (section 2) and the LAN pairing first.

## 6. Order of work

1. Event streams with sequence numbers in the server, and replay on attach.
2. Move `ShellPool` and the PTYs into the server, and make the GUI a client. `mrt` auto-starts the server.
3. Layout persistence in the server, and a versioned protocol.
4. Keep the old server alive across upgrades.
5. LAN pairing, encryption and revocation, then the reconnect test.
6. Internet transport, then the mobile companion.

## 7. Verification

- Unit and app tests: `session_host.rs` (jobs survive their client, hosted PTYs replay on reattach) and `app.rs` (a command runs through the host; a new window reattaches a job and a running full-screen program).
- `crates/mrt-app/tests/e2e_processes.rs`: real `mrt server`, `mrt share`, `mrt pair`, `--host`, revocation and `mrt session-host` processes with separate `MRT_DATA_DIR`s.
- `scripts/e2e/gui_crash_reattach.py`: the real window on an offscreen X server (`scripts/e2e/xvfb_offscreen.sh`, no root needed): it types a long command and a full-screen program into a tab, kills the window with SIGKILL, checks that the session host still runs both, starts the window again and checks (over MCP, with screenshots) that the program is back in the same tab with its screen and that both end as blocks there.
