#!/usr/bin/env python3
"""Generate a large two-word evaluation matrix for lay."""

from __future__ import annotations

import csv
from pathlib import Path

from eval_two_word_models import convert


OUT = Path("eval/two_word_cases.tsv")


EN_LEFT = [
    "good",
    "test",
    "word",
    "live",
    "double",
    "check",
    "download",
    "file",
    "code",
    "data",
    "mode",
    "project",
    "normal",
    "smart",
    "fast",
    "slow",
    "linux",
    "gnome",
    "wayland",
    "rust",
    "cargo",
    "buffer",
    "keyboard",
    "layout",
    "helper",
]


RU_LEFT = [
    "привет",
    "текст",
    "слово",
    "тест",
    "проверка",
    "можно",
    "нужно",
    "дальше",
    "хорошо",
    "пример",
    "работа",
    "проект",
    "файл",
    "режим",
    "модель",
    "ошибка",
    "помощник",
    "клавиатура",
    "раскладка",
    "буфер",
    "демон",
    "меню",
    "скорость",
    "логика",
    "правка",
]


RU_TARGETS = [
    "привет",
    "мир",
    "текст",
    "слово",
    "слова",
    "проверка",
    "работа",
    "можно",
    "нужно",
    "дальше",
    "хорошо",
    "плохо",
    "правильно",
    "просто",
    "проект",
    "файл",
    "код",
    "тест",
    "режим",
    "модель",
    "ошибка",
    "помощь",
    "набор",
    "печать",
    "клавиатура",
    "раскладка",
    "буфер",
    "демон",
    "окно",
    "меню",
    "скорость",
    "логика",
    "пример",
    "вопрос",
    "ответ",
    "сегодня",
    "завтра",
    "человек",
    "строка",
    "правка",
]


EN_TARGETS = [
    "good",
    "test",
    "word",
    "live",
    "double",
    "check",
    "download",
    "file",
    "code",
    "data",
    "mode",
    "project",
    "normal",
    "smart",
    "fast",
    "slow",
    "linux",
    "gnome",
    "wayland",
    "rust",
    "cargo",
    "buffer",
    "keyboard",
    "layout",
    "helper",
    "timer",
    "window",
    "status",
    "service",
    "option",
    "memory",
    "branch",
    "commit",
    "github",
    "readme",
]


RU_KEEP_PAIRS = [
    ("проверка", "слова"),
    ("проверка", "текста"),
    ("слово", "работает"),
    ("тест", "прошёл"),
    ("можно", "дальше"),
    ("нужно", "проверить"),
    ("код-дэ-вуар", "тест"),
    ("схема", "таможенник"),
    ("пошли", "в"),
    ("в", "доме"),
    ("ну", "да"),
    ("не", "надо"),
    ("это", "нормально"),
    ("всё", "работает"),
    ("очень", "быстро"),
    ("новая", "модель"),
    ("маленькая", "модель"),
    ("русский", "текст"),
    ("два", "слова"),
    ("три", "слова"),
]


EN_KEEP_PAIRS = [
    ("good", "test"),
    ("good", "word"),
    ("double", "word"),
    ("live", "mode"),
    ("check", "text"),
    ("download", "file"),
    ("normal", "mode"),
    ("project", "timer"),
    ("code", "data"),
    ("rust", "cargo"),
    ("linux", "wayland"),
    ("gnome", "shell"),
    ("smart", "helper"),
    ("fast", "path"),
    ("slow", "model"),
    ("branch", "commit"),
    ("github", "readme"),
    ("keyboard", "layout"),
    ("buffer", "window"),
    ("service", "status"),
]


MIXED_KEEP_PAIRS = [
    ("wi-fi", "подключил"),
    ("usb", "кабель"),
    ("API", "работает"),
    ("CPU", "быстрый"),
    ("README", "файл"),
    ("GitHub", "проект"),
    ("AmoCRM", "задача"),
    ("GNOME", "панель"),
    ("Wayland", "сессия"),
    ("Rust", "код"),
    ("cargo", "тест"),
    ("Qwen", "модель"),
    ("BitNet", "эксперимент"),
    ("smollm", "модель"),
    ("LLM", "арбитр"),
    ("USB", "порт"),
    ("HTTP", "запрос"),
    ("JSON", "лог"),
    ("DBus", "мост"),
    ("uinput", "ввод"),
]


UNFINISHED_FRAGMENT_CASES = [
    ("chec тест", "срус тест", "first word current fragment must flip as typed"),
    ("Rjhjxt тест", "Короче тест", "first word current fragment with shift must flip"),
    ("ghjie нет", "прошу нет", "first word current fragment should flip"),
    ("NTVF new", "ТЕМА new", "first word current fragment should flip"),
    ("downl файл", "вщцтд файл", "unfinished English fragment flips as-is"),
]


MIXED_NOISE_CASES = [
    ("пgfhf-gfhf вот", "пара-пара вот", "duplicate layout prefix around hyphen word"),
    ("цwi-fi подключил", "wi-fi подключил", "duplicate Cyrillic prefix before ASCII technical token"),
    ("ЦWi-fi подключил", "Wi-fi подключил", "duplicate Cyrillic prefix before ASCII technical token"),
    ("пщщв ntrcn", "good текст", "first wrong to English, second wrong to Russian"),
    ("good Вщгиду", "good Double", "first English, second wrong English typed RU"),
    ("проверка ntrcn", "проверка текст", "first Russian, second wrong Russian typed US"),
]


BRANDS = ["AmoCRM", "GitHub", "GNOME", "Wayland", "Rust", "Qwen", "BitNet", "API", "CPU", "LLM"]


Row = tuple[str, str, str, str, str]


def row(category: str, typed: str, expected: str, note: str, current_token: str = "none") -> Row:
    return (category, typed, expected, note, current_token)


def rows() -> list[Row]:
    out: list[Row] = []

    for left in EN_LEFT:
        for target in RU_TARGETS:
            typed = f"{left} {convert(target, 'ru2us')}"
            expected = f"{left} {target}"
            out.append(row("en_left_ru_target", typed, expected, "english left, russian target typed in US layout", "last"))

    for left in RU_LEFT:
        for target in EN_TARGETS:
            typed = f"{left} {convert(target, 'us2ru')}"
            expected = f"{left} {target}"
            out.append(row("ru_left_en_target", typed, expected, "russian left, english target typed in RU layout", "last"))

    for left in RU_TARGETS[:24]:
        for right in RU_TARGETS[1:25]:
            typed = f"{convert(left, 'ru2us')} {convert(right, 'ru2us')}"
            expected = f"{left} {right}"
            out.append(row("both_wrong_to_ru", typed, expected, "both words typed in US layout, target Russian", "last"))

    for left in EN_TARGETS[:24]:
        for right in EN_TARGETS[1:25]:
            typed = f"{convert(left, 'us2ru')} {convert(right, 'us2ru')}"
            expected = f"{left} {right}"
            out.append(row("both_wrong_to_en", typed, expected, "both words typed in RU layout, target English", "last"))

    for left, right in RU_KEEP_PAIRS:
        out.append(row("normal_keep_ru", f"{left} {right}", f"{left} {right}", "normal Russian should stay"))

    for left, right in EN_KEEP_PAIRS:
        out.append(row("normal_keep_en", f"{left} {right}", f"{left} {right}", "normal English should stay"))

    for left, right in MIXED_KEEP_PAIRS:
        out.append(row("normal_keep_mixed", f"{left} {right}", f"{left} {right}", "mixed technical or brand token should stay"))

    for brand in BRANDS:
        out.append(row("brand_single_letter", f"{brand} Z", f"{brand} Я", "brand plus single wrong-layout letter", "last"))
        out.append(row("brand_single_letter", f"{brand} Н", f"{brand} Н", "brand plus normal Russian single letter"))

    for typed, expected, note in UNFINISHED_FRAGMENT_CASES:
        out.append(row("unfinished_last_fragment", typed, expected, note, "first"))

    for typed, expected, note in MIXED_NOISE_CASES:
        out.append(row("mixed_noise", typed, expected, note))

    return dedupe(out)


def dedupe(items: list[Row]) -> list[Row]:
    seen: set[tuple[str, str, str]] = set()
    out: list[Row] = []
    for item in items:
        key = (item[1], item[2], item[4])
        if key in seen:
            continue
        seen.add(key)
        out.append(item)
    return out


def main() -> None:
    OUT.parent.mkdir(parents=True, exist_ok=True)
    generated = rows()
    with OUT.open("w", encoding="utf-8", newline="") as f:
        writer = csv.writer(f, delimiter="\t", lineterminator="\n")
        writer.writerow(["id", "category", "typed", "expected", "current_token", "note"])
        for idx, (category, typed, expected, note, current_token) in enumerate(generated, start=1):
            writer.writerow([f"{idx:04d}", category, typed, expected, current_token, note])
    print(f"wrote {OUT} with {len(generated)} cases")


if __name__ == "__main__":
    main()
