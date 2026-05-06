<div align="center">

# lay

**Double Shift layout rescue for Linux/GNOME Wayland**

Typed a word in the wrong keyboard layout? Press **Shift twice** and keep typing.

[![Rust](https://img.shields.io/badge/Rust-1.75+-orange?logo=rust)](https://www.rust-lang.org/)
[![GNOME](https://img.shields.io/badge/GNOME-45--47%2C%2050-4A86CF?logo=gnome)](https://gnome.org/)
[![Wayland](https://img.shields.io/badge/Wayland-native-blue)](https://wayland.freedesktop.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](#license)

</div>

`lay` is a lightweight keyboard helper for Linux users who type in two layouts,
especially RU/EN. Its main feature is simple:

```text
Typed:   Ghbdtn
Press:   Shift Shift
Result:  Привет
```

It does not try to guess everything while you type. The default workflow is
manual and predictable: make a layout mistake, press double Shift, and `lay`
retypes the last word in the other layout.

`lay` is built for GNOME Wayland. It uses Rust, evdev/uinput, and a small GNOME
Shell extension for layout switching.

## Features

- Double Shift fixes the last word typed in the wrong layout.
- Works directly in applications, without clipboard-based replacement.
- GNOME Wayland support through a small Shell extension.
- Fast Rust CLI for one-off conversion and scripts.
- Optional conservative typing assist after Space.
- Optional exact auto-replace rules.
- Local-first design: no cloud service and no network call in the normal path.

## Status

This is an early public/beta project.

The primary tested setup is Ubuntu/GNOME on Wayland with RU/EN layouts. The
extension declares GNOME Shell 45, 46, 47, and 50 support. Other GNOME versions,
distributions, KDE, Sway, Hyprland, and non-RU layouts may need extra work.

Bug reports and real typing examples are welcome, but please remove private
text before sharing logs or reproduction cases.

## Quick Start

```bash
git clone https://github.com/radislabus-star/lay.git ~/projects/lay
cd ~/projects/lay
bash install.sh
```

After installation, log out and log back in so the `input` group and GNOME
extension are picked up.

Then type a word in the wrong layout and press **Shift twice**.

## Requirements

- Linux
- GNOME Shell 45, 46, 47, or 50
- Wayland session
- Rust 1.75+
- Access to `/dev/input` through the `input` group
- `uinput` support

The installer can add the current user to the `input` group, but the group
change only applies after a new login session.

## CLI

`lay` can also convert text from the terminal:

```bash
lay "Ye djn ghbvth"
# Ну вот пример

lay "руддщ цщкдв"
# hello world

echo "ghbdtn" | lay
# привет

lay --clipboard
```

The CLI is useful for scripts, quick checks, and clipboard conversion.

## Daemon

`lay-daemon` is the background service that makes double Shift work in real
applications.

Useful commands:

```bash
systemctl --user status lay-daemon --no-pager
systemctl --user restart lay-daemon
systemctl --user stop lay-daemon
journalctl --user -u lay-daemon -n 120 --no-pager
```

## GNOME Extension

The daemon reads physical keyboard events and replays keycodes, but layout
switching on GNOME Wayland requires GNOME Shell integration. The extension lives
in:

```text
extension/lay@radislabus-star.github.io/
```

The installer copies it to:

```text
~/.local/share/gnome-shell/extensions/lay@radislabus-star.github.io/
```

## Tray Menu

The GNOME tray menu keeps the public path short:

- `Main`: enable the standard Double Shift trigger
- `Assist`: typing assist and exact auto-replace
- `Data`: opt-in saving of accepted corrections
- `Service`: daemon stop/start
- `Advanced`: optional LLM mode, 1/2-word scope, timing, and alternative triggers

Production mode does not expose a text log button.

## How It Works

When double Shift is detected:

```text
physical keyboard -> evdev -> lay-daemon
                              |
                              v
                       current word buffer
                              |
                              v
                    Backspace x word length
                              |
                              v
                 GNOME extension switches layout
                              |
                              v
                  uinput replays original keycodes
```

This means the same physical keys are typed again under the other layout. That
is why `Ghbdtn` becomes `Привет` without touching the clipboard.

## Optional Typing Assist

`lay` can also run a conservative helper after Space. This is separate from the
main double Shift workflow.

Typing assist is designed to be quiet:

- it checks only completed words;
- it fixes only high-confidence local mistakes;
- it uses exact rules, dictionaries, and a small char n-gram scorer;
- it does not rewrite style or generate new text;
- if it is not sure, it does nothing.

Examples of intended corrections:

```text
ошисбя -> ошибся
я вно  -> явно
плозо  -> плохо
```

Enable or disable it from the tray menu:

```json
{
  "typing_assist": true
}
```

## Auto-Replace Rules

Exact replacements can be configured in:

```text
~/.config/lay/replacements.json
```

Example:

```json
{
  "подлючись": "подключись",
  "Надйи": "Найди"
}
```

This feature is intentionally exact. Fuzzy matching belongs to typing assist,
not to the replacement dictionary.

## Privacy

Keyboard tools deserve extra suspicion. `lay-daemon` sees keyboard events, so
the project tries to keep the data model boring and local.

By default, `lay` does not send typed text anywhere. The normal double Shift
path does not require network access, cloud APIs, or a remote model.

Optional learning logs are local and should contain accepted correction pairs,
not the full stream of typed text. They are disabled by default and can be
enabled from the tray menu with `Data -> Remember corrections`:

```text
~/.local/share/lay/corrections.jsonl
```

Diagnostic output is also disabled by default. Developers can enable it
explicitly with `lay-daemon --debug-log` or `LAY_DEBUG_LOG=1`.

The GNOME extension exposes a session-local DBus bridge so `lay-daemon` can
switch layouts and insert fallback text. This is not a security boundary against
other processes running as the same desktop user. Public input methods are kept
small: direct `Backspace` and `ReplaceLastN` DBus methods are not exported.

You can stop the daemon at any time:

```bash
systemctl --user stop lay-daemon
```

## Smart/LLM Mode

There is an experimental `--smart` mode that can use a local model as an
arbiter between prepared candidates.

It is not the main product path, it is not required for double Shift, and it is
not enabled for normal typing rescue.

Tray `Advanced -> LLM` affects only `lay-daemon`. The terminal CLI uses model
logic only when `--smart` is passed.

The default build does not compile the direct GGUF backend and does not load a
model at startup. To use Ollama for experiments:

```bash
LAY_LLM_BACKEND=ollama lay --smart "fyukbqcrbq"
```

To build the optional direct GGUF backend:

```bash
cargo build --release --features direct-llm
LAY_LLM_BACKEND=direct LAY_GGUF_MODEL=/path/to/model.gguf lay --smart "fyukbqcrbq"
```

```bash
lay --smart "fyukbqcrbq"
```

## Development

```bash
cargo test
cargo build --release
```

N-gram corpus helpers:

```bash
cargo run --bin lay-ngram-corpus -- check-cache
cargo run --bin lay-ngram-corpus -- check --corpus corpus/ru_50mb.txt
```

Install the current build locally:

```bash
bash install.sh
```

## Roadmap

- Better public installer and uninstall command.
- A short demo GIF/video for the double Shift workflow.
- More regression tests from accepted/rejected real corrections.
- Safer defaults and clearer privacy controls.
- KDE/Sway/Hyprland research.
- More layouts after RU/EN is stable.

## License

MIT
