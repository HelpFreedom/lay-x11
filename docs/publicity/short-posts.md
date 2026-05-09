# Short posts

## Russian Telegram / Linux chat

Сделал маленький open-source помощник для GNOME Wayland: `lay`.

Сценарий простой: набрал слово не в той RU/EN раскладке, нажал Shift два раза,
слово перепечаталось в другой раскладке.

```text
ghbdtn      -> привет
good ntrcn -> good текст
wi-fi ye   -> wi-fi ну
```

Работает локально: Rust daemon + evdev/uinput + маленькое GNOME Shell
extension для переключения раскладки. Обычный double Shift не использует облако,
LLM или буфер обмена.

Пока это beta под GNOME Wayland и RU/EN. Буду рад коротким воспроизводимым
багам и идеям.

GitHub:
https://github.com/radislabus-star/lay-public

## Habr intro teaser

Я несколько недель доводил до рабочего состояния маленькую утилиту для GNOME
Wayland: нажимаешь Shift два раза, и слово, набранное не в той раскладке,
перепечатывается правильно.

Самое интересное оказалось не в `ghbdtn -> привет`, а в пограничных случаях:
`good ntrcn`, `AmoCRM Z`, `wi-fi ye`, частичные слова, автопомощь после пробела
и отказ от агрессивной LLM-магии.

Ниже технический разбор архитектуры, ошибок и решений.

## Reddit / r/gnome

I built a small local keyboard helper for GNOME Wayland because I kept typing
Russian text in the English layout.

It listens to physical key events, keeps a small word buffer, and on double
Shift replays the same keycodes under the other layout. The normal path does not
use the clipboard or a cloud service.

Examples:

```text
ghbdtn      -> привет
good ntrcn -> good текст
wi-fi ye   -> wi-fi ну
```

It is still beta and mostly RU/EN-focused, but it is already useful for my daily
typing. I would appreciate feedback from GNOME Wayland users.

Repo:
https://github.com/radislabus-star/lay-public

Disclosure: I am the author.

## DEV.to / social intro

I built `lay`, a local double-Shift layout rescue tool for GNOME Wayland.

The fun part was not converting `ghbdtn` to `привет`, but avoiding damage in
mixed text like `good ntrcn`, `AmoCRM Z`, and `wi-fi ye`.

Rust daemon, evdev/uinput, GNOME Shell extension, local-first, no cloud required
for the normal path.

https://github.com/radislabus-star/lay-public

