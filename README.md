# lay (X11)

**Punto/Caramba-style переключатель раскладки для Linux X11.**
Печатаешь не той раскладкой? Жмёшь Shift→Shift — последнее слово
перепечатывается правильно, раскладка системы переключается.

Форк [radislabus-star/lay-public](https://github.com/radislabus-star/lay-public)
для X11. Оригинал работает только на GNOME Wayland (требует GNOME Shell extension).
Эта версия — pure Rust, без зависимости от GNOME/Wayland, работает на любом
X11-окружении: **DWM, i3, Openbox, KDE/X11, XFCE, Cinnamon, MATE**.

## Что делает

- **Двойной Shift** → перепечатывает последнее слово в другой раскладке и
  переключает её. Работает в чате, браузере, терминале, sandbox-приложениях
  (Flatpak/Snap) — потому что слушает клавиатуру через `/dev/input/event*` и
  печатает через `uinput`, минуя X.
- **Помощь при наборе** (опционально) — после пробела сам исправляет слово,
  если уверен (по словарю `replacements.json` + n-gram scorer + hunspell-ru).
  Например: `ghbdtn ` → `привет `, `wifi ` → `Wi-Fi `, `вобще ` → `вообще `.
- **Точные замены брендов/терминов** — `wifi → Wi-Fi`, `github → GitHub`,
  `вобщем → в общем`. Список в `~/.config/lay/replacements.json`.
- **Список «не трогать»** — `~/.config/lay/no_replace.txt`. Команды терминала
  (`cd`, `ls`, `git`) и твои алиасы никогда не подменяются.
- **Runtime-переключение авто-режима** жестом `Shift→Ctrl→Shift→Shift`.
  По умолчанию (после reboot) — базовый режим без авто-исправлений. Сделал жест
  → включились все три авто-опции (typing_assist, auto_replace, auto_switch_layout).
  Сделал ещё раз — выключились. Конфиг при этом не меняется.

## Что изменено относительно оригинала

| | Оригинал ([lay-public](https://github.com/radislabus-star/lay-public)) | Этот форк |
|---|---|---|
| Платформа | GNOME Wayland | **Любой X11 WM** |
| Переключение раскладки | DBus → GNOME Shell extension → `inputSources[i].activate()` | **`XkbLockGroup` через x11rb** (синхронно, без FFI) |
| Чтение текущей раскладки | DBus → extension | **`XkbGetState`** |
| Fallback ввод текста | DBus → Clutter virtual device / `inputMethod.commit()` | `xdotool type` (опционально, обычно не нужен) |
| ibus engine sync | `ibus engine xkb:ru::rus` | удалено |
| Tray UI | GNOME Shell extension (~800 строк JS) | удалено — конфиг через `~/.config/lay/*.json` |
| LLM smart-режим | Ollama / direct GGUF | удалён (стаб); replay-путь работает |
| Зависимости | `zbus`, `llama_cpp`, GNOME 45–50 | **только `x11rb`** (pure Rust) |
| Runtime-флаги авто-режима | через tray menu, пишется в config | **жестом Shift→Ctrl→Shift→Shift, в памяти; reboot сбрасывает** |
| Protection list для команд | нет | **`no_replace.txt`** — `cd`/`ls`/`git`/алиасы не трогаются |
| `correct_split_word_pair` глюк | «привет и» → «привети» (баг suffix-эвристики) | **исправлено**: одиночные служебные буквы (и, я, а, в, к, с, у, о, ю) не приклеиваются |

Сохранены без изменений: WordBuffer, FSM двойного Shift, словарь QWERTY↔ЙЦУКЕН,
ngram scorer, эвристики исправлений, learning log с pending-feedback, точные
замены.

## Установка

### Зависимости системы

```bash
sudo apt install hunspell-ru     # для распознавания русских слов в typing-assist
sudo apt install xdotool         # опционально, fallback ввод
sudo usermod -aG input $USER     # для чтения /dev/input/event*
# перелогинься чтобы группа применилась
```

### Сборка и установка

```bash
git clone https://github.com/HelpFreedom/lay-x11
cd lay-x11
./install.sh
```

`install.sh`:
- собирает release-бинарники (~2 МБ суммарно),
- ставит симлинки `~/.local/bin/{lay,lay-daemon}`,
- кладёт systemd-юнит в `~/.config/systemd/user/lay-daemon.service`,
- копирует example-конфиги в `~/.config/lay/` (если их ещё нет).

### Запуск daemon

```bash
systemctl --user enable --now lay-daemon
journalctl --user -u lay-daemon -f       # смотреть лог
```

Или в форграунде для отладки:
```bash
DISPLAY=:0 LAY_DEBUG_LOG=1 ~/.local/bin/lay-daemon
```

## Использование

### CLI

```bash
lay "Ye djn ghbvth"           # → Ну вот пример
lay "руддщ цщкдв"             # → hello world
lay --clipboard               # конвертирует то, что в буфере
echo "ghbdtn" | lay           # → привет
```

### Daemon

- **Двойной Shift** (по умолчанию) — перепечатать последнее слово в другой раскладке.
  Настраивается в `~/.config/lay/config.json` — `double-ctrl`, `double-alt`,
  `caps-lock`, `single-rshift`, `single-rctrl`, `single-ralt`, `single-pause`.
- **Жест `Shift → Ctrl → Shift → Shift`** (4 тапа в течение ~3 сек) — включить/выключить
  авто-режим. В журнале появляется `⚙ AUTO ON` / `⚙ AUTO OFF`.

### Конфиг

`~/.config/lay/config.json`:

```json
{
  "trigger": "double-lshift",
  "tap_max_ms": 200,
  "shift_window_ms": 250,
  "debounce_ms": 50,
  "replace_words": 1
}
```

В этом форке поля `typing_assist`, `auto_replace`, `auto_switch_layout`,
`learning_log` **из конфига не читаются** — управляются только жестом.

### Свой словарь подмен

`~/.config/lay/replacements.json` — список «опечатка → правильно»:

```json
{
  "wifi": "Wi-Fi",
  "github": "GitHub",
  "вобщем": "в общем",
  "потомучто": "потому что"
}
```

После правки — рестарт daemon. Кейс умный: ключ `github` сработает и на `GitHub`/`GITHUB`.

### Список «не трогать»

`~/.config/lay/no_replace.txt` — токены которые daemon **никогда** не подменяет
и не переключает раскладку:

```
cd
ls
git
nv         # твой алиас для nvim
gp         # твой алиас
```

Один токен в строке, регистр игнорируется, `#` — комментарий. После правки —
рестарт daemon.

## Архитектура

```
physical keyboard
  └── /dev/input/event*  (evdev)
       └── lay-daemon
            ├── WordBuffer            (буфер физических нажатий)
            ├── DShiftState FSM       (детектор двойного Shift)
            ├── GestureState FSM      (детектор Shift→Ctrl→Shift→Shift)
            ├── typing-assist         (после пробела, optional)
            ├── uinput Backspace + replay
            └── x11_layout::lock_group(group)   ← XkbLockGroup (синхронный)
```

Слой X11 в `src/x11_layout.rs` (~130 строк):
- `XkbLockGroup` — переключение группы раскладки
- `XkbGetState` — чтение активной группы
- `XTest fake_input` — эмуляция клавиш (для fallback)

Через `x11rb` (pure Rust, без libxcb-FFI). Бинарь зависит только от libc и
системного X-сервера.

## Размер

| Бинарь | Размер | Зависимости |
|---|---|---|
| `lay` | ~800 КБ | clipboard, dict |
| `lay-daemon` | ~1.2 МБ | evdev, uinput, x11rb |

## Тесты

```bash
cargo test --release --bin lay-daemon
```

В X11-форке 78 тестов проходят. 12 падают — это тесты оригинала, которые
ожидают рабочий LLM-арбитр; в этом форке LLM стабнут.

## Известные ограничения

- **Только RU↔EN.** Чтобы добавить UK/DE/FR — нужно дописать таблицы в `dict.rs`
  и константы `X11_GROUP_*` в `lay_daemon.rs` (порядок групп зависит от
  `setxkbmap -layout us,ru,...`).
- **`X11_GROUP_US=0`, `X11_GROUP_RU=1`** зашиты в код. Если у тебя другой
  порядок — поправь две константы в `src/bin/lay_daemon.rs`.
- **LLM smart-режим в этом форке отключён.** Если нужен — можно вернуть
  `src/llm.rs` из upstream и добавить `zbus`/`llama_cpp` в `Cargo.toml`.
- **Wayland не поддерживается.** Для Wayland есть оригинальный
  [radislabus-star/lay-public](https://github.com/radislabus-star/lay-public) с
  GNOME Shell extension.

## Лицензия

MIT — как у оригинала.

## Кредиты

- **Изначальная идея и архитектура daemon, WordBuffer, FSM, ngram, словари,
  learning log:** [radislabus-star/lay-public](https://github.com/radislabus-star/lay-public).
- **X11-порт, gesture-toggle, no_replace, фикс склейки одиночных служебных слов:**
  этот форк.
