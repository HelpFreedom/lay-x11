# Как это работает

## Задача

На Windows есть Caramba/Punto Switcher: случайно набрал текст в неправильной
раскладке (`Ye djn ghbvth` вместо `Ну вот пример`), нажимаешь горячую клавишу —
и слово автоматически конвертируется. На Linux нативного аналога нет
(xneur умер в 2017, Wayland не поддерживается).

`lay` решает эту задачу двумя инструментами:

| Инструмент | Что делает |
|---|---|
| **`lay`** | CLI — конвертирует текст аргументом или из stdin/clipboard |
| **`lay-daemon`** | Фоновый демон — двойной Shift прямо в любом приложении |

---

## lay-daemon — как работает двойной Shift

### Общий поток

```
Физическая клавиатура
        │  evdev (/dev/input/event*)
        ▼
  lay-daemon (Rust)
  ┌──────────────────────────────────────┐
  │                                      │
  │  WordBuffer                          │
  │  Накапливает (keycode, shift)        │
  │  для текущего слова.                 │
  │  Сбрасывается на Enter/Tab/Space/←→  │
  │                                      │
  │  FSM двойного Shift                  │
  │  Idle → FirstPress                   │
  │       → WaitingSecond                │
  │       → SecondPress                  │
  │       → DOUBLE SHIFT!                │
  │                                      │
  └──────────────┬───────────────────────┘
                 │ DOUBLE!
                 ▼
  ┌──────────────────────────────────────┐
  │  handle_double_shift                 │
  │                                      │
  │  1. uinput Backspace × N             │
  │     (стереть слово с экрана)         │
  │                                      │
  │  2. DBus → GNOME Extension           │
  │     ActivateLayout('ru'/'us')        │
  │     SetGlobalEngine (ibus tray)      │
  │                                      │
  │  3. uinput replay keycodes           │
  │     (те же keycodes → новая          │
  │      раскладка → другие символы)     │
  └──────────────────────────────────────┘
```

### Почему evdev, а не X11/Wayland input grabbing

На **Wayland** приложения полностью изолированы друг от друга по вводу.
Нет глобального `XGrabKey`, нет `GetKeyboardState`. Единственный
способ читать все нажатия — `/dev/input/event*` (evdev), напрямую
из ядра, до любых Wayland-протоколов.

Требует членства в группе `input`:
```bash
sudo usermod -aG input $USER
```

### Почему uinput, а не wtype/xdotool

`wtype` и `xdotool` работают через протоколы Wayland/X11 и не принимаются
sandbox-изолированными приложениями (Flatpak, snap, GNOME Terminal и др.).

`uinput` — виртуальное устройство ввода в ядре Linux. С точки зрения
системы это **физическая клавиатура**. Получают события все приложения,
включая sandbox-изолированные.

### FSM детектора двойного Shift

Простого «два release подряд» недостаточно: если держать Shift для
заглавной буквы, а потом отпустить — это тоже release. Нужен полный цикл
**press → release → press → release**:

```
Idle
 │ LShift press
 ▼
FirstPress { pressed_at }
 │ release, удержан ≤ 200ms (тап)      release > 200ms → Idle
 ▼                                      (держали = заглавная буква)
WaitingSecond { first_release }
 │ LShift press, < 500ms от release    press > 500ms → FirstPress (новый цикл)
 ▼
SecondPress { second_press }
 │ release, удержан ≤ 200ms (тап)      release > 200ms → Idle
 ▼
DOUBLE SHIFT ✓
```

Любая **другая клавиша** в состояниях FirstPress/WaitingSecond → Idle (отмена).
В состоянии SecondPress — игнорируется (второй Shift уже нажат, ждём release).

### Почему GNOME Extension для переключения раскладки

На Wayland нет API для переключения раскладки извне. `gsettings` меняет
настройки, но не применяет их к текущей сессии ([Bug #1956916](https://gitlab.gnome.org/GNOME/gnome-shell/-/issues/1956916)).
Виртуальная эмуляция `Alt+Shift` не регистрируется Mutter как акселератор.

Единственный работающий способ — вызвать `inputSources[i].activate()`
**внутри GNOME Shell** (GJS). Поэтому есть extension, который экспортирует
этот вызов через DBus:

```
lay-daemon → gdbus call → org.gnome.Shell
                           → extension.js
                              → inputSources[i].activate()  ✓
```

---

## lay CLI — двухступенчатая логика

### Ступень 1 — словарная конвертация

Каждой клавише QWERTY соответствует ровно одна клавиша ЙЦУКЕН — **биекция**:

```
q↔й  w↔ц  e↔у  r↔к  t↔е  y↔н  u↔г  i↔ш  o↔щ  p↔з
a↔ф  s↔ы  d↔в  f↔а  g↔п  h↔р  j↔о  k↔л  l↔д
z↔я  x↔ч  c↔с  v↔м  b↔и  n↔т  m↔ь
```

`HashMap<char, char>` + `OnceLock`. Конвертация — микросекунды.
Покрывает **95% случаев**.

### Ступень 2 — оценка качества (без внешних словарей)

После конвертации проверяем результат через эвристики:
- хотя бы одна гласная на каждые 5-6 букв
- не больше 3 согласных подряд

Если score < threshold (default 0.7) или оба варианта выглядят правдоподобно
(`hello` ↔ `руддщ`), переходим к ступени 3.

Реальные wordlist'ы весят 10-30 МБ, а нам нужен мгновенный ответ.
Эвристика даёт ~85% точность для типичных случаев.

### Режим 3 — optional LLM arbiter

Модель не является основным путём работы. По умолчанию публичная сборка не
загружает LLM и не компилирует direct GGUF backend.

Простой режим LLM не вызывает. LLM включается только явно:
- в daemon: через `Advanced → LLM` в tray menu;
- в CLI: только через флаг `--smart` и выбранный `LAY_LLM_BACKEND`.

`lay-daemon` прогревает smart engine только если стартует со smart/LLM engine в
config. Обычный CLI при этом не начинает использовать модель сам по себе.

Поддерживаемые экспериментальные backend'ы:

```bash
LAY_LLM_BACKEND=ollama lay --smart "fyukbqcrbq"
cargo build --release --features direct-llm
LAY_LLM_BACKEND=direct LAY_GGUF_MODEL=/path/to/model.gguf lay --smart "fyukbqcrbq"
```

Объём исправления (`Advanced → 1 слово` / `Advanced → 2 слова`) независим от
engine. В `Simple + 2 слова` daemon делает физический replay двух слов. В
`LLM + 2 слова` он может выбрать tokenwise-результат, например оставить первое
нормальное слово и исправить второе.

Перед моделью есть быстрый deterministic repair для смешанного текста: если в
русском слове остались латинские клавиши, они домаппятся в RU, а
ASCII-акронимы вроде `LLM` сохраняются. Для чистой обычной перекладки
используется replay-путь, чтобы второй double-Shift мог вернуть текст обратно.

Модель не генерирует исправленный текст с нуля. Код сначала строит
детерминированные кандидаты (`оригинал` и `US↔RU`), а модель выбирает один из
них коротким ответом `A`/`B`. Если backend выключен или ответ не распознан,
используется deterministic fallback.

### Учебный лог

После успешного double-Shift daemon добавляет строку JSONL в
`~/.local/share/lay/corrections.jsonl`. Пишутся только явные исправления:
`from`, `to`, `kind`, `replace_words`, `words`, `ts`. Обратный replay-toggle не
логируется, чтобы не добавлять пары нормального текста в мусорную раскладку.

Лог ограничен: при размере больше 1 MB файл подрезается до последних 3000 строк.

### Автоподмена простого режима

Если в config включено `auto_replace`, простой режим после обычной перекладки
проверяет результат по точным правилам автоподмены. Это не LLM и не fuzzy-поиск:
правило срабатывает только при точном совпадении слова/фразы. Встроенные правила
дополняются пользовательским JSON-словарём `~/.config/lay/replacements.json`.

```
Choose the normal text, not keyboard-layout garbage.
A hello B руддщ => A
A руддщ B hello => B
A ghbdtn B привет => B
A привет B ghbdtn => A
A fyukbqcrbq B английский =>
```

### DBus и модель доверия

GNOME extension экспортирует session-local DBus bridge для `lay-daemon`.
Публичными оставлены только методы, которые нужны runtime-пути: `ActivateLayout`,
`CurrentLayout`, `NextLayout`, `ListLayouts`, `TypeText` и `Ping`.

Методы прямого удаления текста (`Backspace`, `ReplaceLastN`) не экспортируются:
обычная очистка слова делается через uinput внутри daemon. `TypeText` остаётся,
потому что он нужен для typing assist и fallback-вставки, если GNOME не подтвердил
переключение раскладки после удаления слова.

DBus внутри user session не является границей безопасности от процессов того же
пользователя. Если пользователь не доверяет локальным процессам в своей сессии,
extension и daemon нужно выключить.

Если переключение раскладки не подтвердилось, daemon не делает blind replay в
старой раскладке. Он пытается вернуть ожидаемый текст через `TypeText`, не
обновляет cache активной раскладки и пишет диагностическое событие только в
debug-log, если он явно включён.

---

## Архитектура кода

```
lay/
├── src/
│   ├── main.rs          — CLI (clap), словарная конвертация и --smart
│   ├── dict.rs          — словарь US↔RU, detect_direction, convert
│   ├── quality.rs       — эвристика качества
│   ├── ngram.rs         — char 3-gram scorer для typing assist
│   └── llm.rs           — optional model arbiter вокруг готовых кандидатов
│
├── src/bin/
│   ├── lay_daemon.rs        — evdev listener, FSM, uinput replay, DBus client
│   └── lay_ngram_corpus.rs  — build/check/cache локального n-gram корпуса
│
└── extension/
    └── lay@radislabus-star.github.io/
        ├── extension.js — DBus service + tray indicator
        └── metadata.json
```

## Зависимости CLI

| Crate | Зачем |
|---|---|
| `clap` | CLI парсинг |
| `arboard` | Clipboard (Wayland + X11) |
| `ureq` | optional HTTP backend для Ollama |
| `llama_cpp` | optional direct GGUF backend (`direct-llm` feature) |
| `serde` / `serde_json` | Config и JSON-запросы |

## Зависимости daemon

| Crate | Зачем |
|---|---|
| `evdev` | Чтение физической клавиатуры и `evdev::uinput` |
| `clap` | CLI парсинг |

## Профиль release

```toml
[profile.release]
opt-level = 3
lto = true           # link-time optimization
codegen-units = 1    # медленнее компиляция, быстрее бинарь
strip = true         # без отладочных символов
panic = "abort"      # без unwinding
```

Бинарник `lay` — 2.6 MB, `lay-daemon` — аналогично.

## Что не умеет (пока)

- **Дополнительные раскладки** (UK, DE, FR) — добавить таблицы в `dict.rs`
- **Автоопределение без двойного Shift** — статистический анализ потока
