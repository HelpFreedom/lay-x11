# Development Notes

## Project

`lay` (X11 fork) is a Caramba/Punto-style layout switcher for Linux/X11.

Originally written for GNOME Wayland (https://github.com/radislabus-star/lay-public).
This fork strips the GNOME Shell extension and replaces all DBus/ibus layer with
direct X11 calls via `x11rb` — pure Rust, no FFI, no GNOME dependency. Works on
any X11 WM (DWM, i3, Openbox, KDE/X11, XFCE, Cinnamon).

The daemon listens to physical keyboard events through `evdev`, stores recent
keycodes, and replays them through `uinput` after a manual trigger or
typing-assist heuristic.

## Runtime Architecture (X11 fork)

```text
physical keyboard
  -> /dev/input/event*  (evdev)
  -> lay-daemon
      -> WordBuffer  (per-keystroke history)
      -> trigger FSM (double-Shift / Ctrl×2 / etc.)
      -> gesture FSM (Shift→Ctrl→Shift→Shift = toggle AUTO mode)
      -> typing-assist  (after space, optional)
      -> uinput Backspace + replay
      -> x11rb XkbLockGroup  (synchronous layout switch)
```

The original GNOME path:
```text
... daemon -> zbus DBus -> GNOME Shell extension -> inputSources[i].activate()
```
... has been replaced with a direct `XkbLockGroup` call. No tray UI, no extension.

## Important Constraints

- Do not use clipboard for daemon corrections.
- Keep simple mode deterministic and fast.
- LLM mode is stubbed in this fork — `src/llm.rs` returns `None` and the smart
  branches fall back to the deterministic replay path.
- Production daemon must not write typed text logs by default.
- Learning logs are opt-in through config.
- Auto-features (typing_assist, auto_replace, auto_switch_layout) are toggled at
  runtime via the gesture; **not read from config in this fork**. Reboot →
  base mode (auto OFF).

## Useful Commands

```bash
cargo fmt --check
cargo test --release --bin lay-daemon
cargo build --release
systemctl --user restart lay-daemon
```

For diagnostics:

```bash
DISPLAY=:0 LAY_DEBUG_LOG=1 ~/.local/bin/lay-daemon   # foreground
journalctl --user -u lay-daemon -f                   # background
```

## Key Modules

- `src/x11_layout.rs` — X11 XKB+XTest backend (pure Rust via `x11rb`)
- `src/bin/lay_daemon.rs` — main daemon (evdev → FSM → uinput, ~7k lines)
- `src/dict.rs` — QWERTY↔ЙЦУКЕН bijection
- `src/ngram.rs` — char 3-gram scorer for typing-assist confidence
- `src/llm.rs` — stub (LLM disabled in X11 build)
- `src/main.rs` — CLI `lay "text"`
- `examples/x11_probe.rs` — quick X11 backend diagnostic
- `examples/config/*.example.*` — starter configs to copy into `~/.config/lay/`
