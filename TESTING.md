# Testing pi-milya (GUI + TUI)

For a human or an agent. Everything below was used for the verified
end-to-end test on this rig (Bazzite, `Qwen3.8-27B` via llamesa).

## Build

```sh
cargo build                 # both binaries: pi-milya (TUI), pi-milya-gui (GUI)
cargo build --release
```

Zero warnings expected. `--smoke` is the TUI's non-UI end-to-end check:

```sh
cargo run -- --smoke        # default-run is pi-milya; explicit form:
 cargo run --bin pi-milya -- --smoke --pi /path/to/pi
```

(`--smoke` prints `model: default` when pi has no explicit model set in
`--no-session` state — that is normal.)

## Interactive GUI (needs a display)

```sh
cargo run --bin pi-milya-gui
# if pi is not on PATH for the launching shell:
cargo run --bin pi-milya-gui --pi /path/to/pi
```

Expected on first run: the wizard form opens (the family starts empty),
model list is fetched live from pi (a throwaway pi process — a few
seconds). Save a member, chat, add a second member with **new member**,
switch with `ctrl+n`/`ctrl+p`, quit with `ctrl+q` (family is saved on
exit; the member's `session_id` is persisted, so its conversation resumes
on the next launch).

> **PATH matters**: the GUI spawns `pi` as a child. If `pi` (a node
> script) is not on the child's `PATH`, it dies with
> `env: 'node': No such file or directory` — that message is shown in the
> chat pane by design (pi's stderr is surfaced as `pi: ...` notes).

## Headless GUI (no display) — verified procedure

Works on this rig (and any Linux box with Xvfb + xdotool + ffmpeg):

```sh
# 1. virtual display
systemd-run --user --unit=xvfb99 Xvfb :99 -screen 0 1280x800x24   # or: Xvfb :99 &

# 2. the GUI (systemd-run so it survives the launching shell; set PATH for node)
systemd-run --user --unit=guitest \
  -p Environment=PATH=$HOME/.local/node/bin:$HOME/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/bin \
  sh -c "cd $HOME/Development/pi-milya && DISPLAY=:99 \
         exec ./target/release/pi-milya-gui --pi $HOME/.local/bin/pi > /tmp/gui.log 2>&1"

# 3. wait ~15s (pi startup), then screenshot
ffmpeg -y -f x11grab -video_size 1280x800 -i :99.0 -frames:v 1 -update 1 out.png

# 4. drive it (no WM on Xvfb: set input focus directly, use XTEST keys)
W=$(DISPLAY=:99 xdotool search --name pi-milya | head -1)
DISPLAY=:99 xdotool mousemove 400 710 click 1          # focus the input
DISPLAY=:99 xdotool windowfocus --sync $W
DISPLAY=:99 xdotool type --delay 60 "say the word: pong"
DISPLAY=:99 xdotool key Return
# 5. wait ~40s for the model reply, screenshot again
```

Caveats learned the hard way:
- `xdotool type --window` (XSendEvent) is **ignored by winit**; use
  `windowfocus --sync` + bare `xdotool type` (XTEST).
- `ImageMagick import -window root` returns black here (gamescope owns the
  real pixels on the user display) — `ffmpeg -f x11grab` is the reliable
  capture for both the virtual and (if you're in a normal session) real
  displays.
- Launch the GUI via `systemd-run --user` (or `nohup`); a bare `&` from a
  tool/agent shell gets reaped when the shell exits.

## TUI

`cargo run` in a real terminal. First run opens the 5-step wizard
(name → model → thinking → face → prompt). `ctrl+n`/`ctrl+p` switch
members; `/new` and `/edit` re-run the wizard; `ctrl+c` quits (pi is
shut down via stdin close).

## What a passing GUI test looks like

1. First run → wizard form, model list populated from pi
2. Save a member → sidebar shows face + name + **the model's display
   name from `get_state`** (e.g. `Qwen3.8-27B (MXFP4, Radiance, 1st GPU)`
   truncated to 36 chars), status dot goes idle (○)
3. Type a prompt + enter → `you> ...` appears, thinking streams, reply
   renders (markdown), dot goes idle
4. `new member` → second member; `ctrl+n` switches, both tabs keep their
   own scrollback; background streams continue while the other tab is open
5. `edit member` → prefilled form; saving restarts that member's pi process
6. Quit (ctrl+q or window close) → `family.json` updated with the active
   member + session ids; relaunch resumes the conversation

## Windows (verified 2026-11-14, full checklist PASS)

- **Toolchain**: if the machine's MSVC/VS 2022 install is incomplete (no
  `lib\x64`, no Windows SDK 10, no CRT headers) or a Git Bash `/usr/bin/link`
  shadows MSVC's `link.exe`, build with the GNU toolchain instead:
  `rustup run 1.99.1-x86_64-pc-windows-gnu cargo build` (MSYS2). The code
  builds warning-free on both msvc and windows-gnu.
- **pi**: when launching from Git Bash, the child process may not find `pi`
  (bash-style `/c/...` PATH entries are unusable to a Windows child) — use
  `--pi C:\Users\<you>\AppData\Roaming\npm\pi.cmd`.
- **Config**: `family.json` lives at `%APPDATA%\pi-milya\family.json`; a
  legacy `~\.config\pi-milya\family.json` (XDG-style) is honored when it
  already exists. An unset `HOME` no longer panics.
- **Verified**: build → smoke → wizard (live model list) → chat round-trip
  (pong, streamed thinking, markdown) → two members + ctrl+n (background
  stream survives tab switches) → edit member (prefill + restart) → quit /
  relaunch (family.json loads, `session_id` resumes — the model answered a
  question about the pre-quit conversation).

## Known-good / gotchas (from the 2026-11-14 build)

- The GUI must keep its tokio `Runtime` alive (`rt_keep` field) — a bare
  `Handle` clone lets the runtime drop and **all background tasks die
  silently** (symptom: stuck at "… connecting", no error). Fixed; don't
  "simplify" it away.
- egui 0.29 API generation: single-string `RichText` (no span support —
  `LayoutJob` if needed), `TopBottomPanel::bottom`, `ComboBox::show_index`,
  `Modifiers` is fields (`.ctrl`), `eframe::App::on_exit` takes a glow
  context with the default features, `run_native`'s closure returns a
  `Result`.
- pi's `get_state` returns `model` as an object (`id`/`name`) — display
  uses `name`. It is `null` before a model is set (e.g. `--no-session`
  without `-m`); smoke prints `model: default` in that case.
- `/dev/urandom` must be read with fixed-size `read_exact` (a blocking
  `fs::read` hangs forever) — the uuid fallback in `pi_client.rs` does.
- Non-repro (Windows, 1536x960): one session on the pre-redesign build had the
  bottom input bar and top header buttons clipped; the 4bce9f7 re-verify
  renders cleanly at 1536x960. If it ever recurs, resize/maximize the
  window — likely an egui/winit viewport quirk, not a code path. (Same
  laptop: desktop display flips 1536x960 ↔ 2560x1440 mid-session and can
  kill synthetic mouse injection — an environment quirk, not the app.)
- `plywoodsheetcalculator` (unrelated repo) still has `node_modules/`
  committed if you ever wonder about that finding.
