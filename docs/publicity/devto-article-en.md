# I built a local double-Shift layout rescue tool for GNOME Wayland

> Draft for DEV.to or a personal engineering blog.
> Repository: https://github.com/radislabus-star/lay-public

I often type in Russian and English in the same workflow. The classic mistake:
I want to type `привет`, but the active layout is English, so I get `ghbdtn`.

On GNOME Wayland this is not as simple as it used to be on X11. You cannot just
grab arbitrary text from every app, mutate it, and push it back. That is good
for security, but inconvenient if you want a tiny local helper similar in spirit
to Punto Switcher or Caramba.

So I built `lay`: a small GNOME Wayland keyboard helper that fixes the last word
when I press Shift twice.

```text
ghbdtn      -> привет
good ntrcn -> good текст
wi-fi ye   -> wi-fi ну
```

The main path is intentionally boring:

```text
physical keyboard
    -> evdev
    -> lay-daemon
    -> word buffer
    -> Backspace x N
    -> GNOME Shell extension switches layout
    -> uinput replays the original keycodes
```

The important part is that the normal double-Shift path does not use the
clipboard and does not need a cloud service. It replays the same physical keys
under the other layout.

The project has two pieces:

- a Rust daemon that listens to evdev and emits uinput events;
- a small GNOME Shell extension that provides tray controls and a DBus bridge
  for layout activation inside GNOME Shell.

The surprisingly hard part was not converting `ghbdtn` into `привет`. The hard
part was not breaking real mixed-language text:

```text
good ntrcn              -> good текст
AmoCRM Z тут задача     -> AmoCRM Я тут задача
Главное Вщгиду          -> Главное Double
```

A naive "flip two words" implementation destroys valid neighboring words. The
current smart mode works token by token: keep good RU/EN tokens, protect ASCII
technical tokens, and only replace the bad range.

I also experimented with tiny local LLMs, but they are not the main path. For a
manual hotkey, predictability matters more than linguistic confidence. If I
press double Shift, I usually want the text flipped, even if the original word
is technically valid English.

The default path is therefore:

- deterministic layout mapping;
- RU/EN dictionaries;
- small heuristics;
- char n-gram scoring;
- optional LLM only as an experimental arbiter.

`lay` also has a conservative typing assist that runs after Space, not on every
key. It catches simple local typos when the signal is strong:

```text
рабоатет   -> работает
ошисбя     -> ошибся
перпаратов -> препаратов
```

Privacy-wise, keyboard tools should be treated with suspicion. The normal path
is local-first: no cloud, no network call, no full keylog. The daemon must see
keyboard events to do its job, so the project tries to keep the data model small
and boring.

Current status:

- beta;
- GNOME Wayland first;
- RU/EN layouts;
- tested primarily on Ubuntu/GNOME;
- GNOME Shell 45, 46, 47 and 50 declared by the extension.

Repository:

https://github.com/radislabus-star/lay-public

If you live between RU/EN layouts on GNOME Wayland, bug reports with short
reproducible examples are especially useful.

