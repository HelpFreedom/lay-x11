#!/usr/bin/env python3
"""Evaluate layered two-word correction candidates for lay.

The script is intentionally outside the daemon hot path. It answers one
question: which layer can choose the expected correction on fixed two-word
cases?

Optional model layers need Python packages:

    uv run --python 3.12 --with torch --with transformers --with sentencepiece \
      scripts/eval_two_word_models.py --layers baseline,tiny2,sage
"""

from __future__ import annotations

import argparse
import csv
import dataclasses
import math
import re
import statistics
import time
from pathlib import Path
from typing import Callable, Iterable


PAIRS = [
    ("q", "й"),
    ("w", "ц"),
    ("e", "у"),
    ("r", "к"),
    ("t", "е"),
    ("y", "н"),
    ("u", "г"),
    ("i", "ш"),
    ("o", "щ"),
    ("p", "з"),
    ("[", "х"),
    ("]", "ъ"),
    ("a", "ф"),
    ("s", "ы"),
    ("d", "в"),
    ("f", "а"),
    ("g", "п"),
    ("h", "р"),
    ("j", "о"),
    ("k", "л"),
    ("l", "д"),
    (";", "ж"),
    ("'", "э"),
    ("z", "я"),
    ("x", "ч"),
    ("c", "с"),
    ("v", "м"),
    ("b", "и"),
    ("n", "т"),
    ("m", "ь"),
    (",", "б"),
    (".", "ю"),
    ("/", "."),
    ("?", ","),
    ("@", '"'),
    ("#", "№"),
    ("$", ";"),
    ("^", ":"),
    ("&", "?"),
    ("`", "ё"),
]


def _build_maps() -> tuple[dict[str, str], dict[str, str]]:
    us_to_ru: dict[str, str] = {}
    ru_to_us: dict[str, str] = {}
    for us, ru in PAIRS:
        us_to_ru[us] = ru
        ru_to_us[ru] = us
        if us.isalpha():
            us_to_ru[us.upper()] = ru.upper()
            ru_to_us[ru.upper()] = us.upper()
    us_to_ru["~"] = "Ё"
    ru_to_us["Ё"] = "~"
    return us_to_ru, ru_to_us


US_TO_RU, RU_TO_US = _build_maps()


@dataclasses.dataclass(frozen=True)
class Case:
    id: str
    category: str
    typed: str
    expected: str
    note: str
    current_token: str = "none"


@dataclasses.dataclass
class Decision:
    layer: str
    case: Case
    output: str
    ok: bool
    ms: float
    detail: str


@dataclasses.dataclass(frozen=True)
class CandidateScore:
    name: str
    text: str
    layout_score: float
    changed: int
    order: int


TOKEN_RE = re.compile(r"\s+|\S+", re.UNICODE)


def has_cyrillic(text: str) -> bool:
    return any("А" <= ch <= "я" or ch in "ёЁ" for ch in text)


def has_latin(text: str) -> bool:
    return any(ch.isascii() and ch.isalpha() for ch in text)


def detect_direction(text: str) -> str:
    cyr = sum(1 for ch in text if "А" <= ch <= "я" or ch in "ёЁ")
    lat = sum(1 for ch in text if ch.isascii() and ch.isalpha())
    return "ru2us" if cyr > lat else "us2ru"


def convert(text: str, direction: str | None = None) -> str:
    direction = direction or detect_direction(text)
    table = RU_TO_US if direction == "ru2us" else US_TO_RU
    return "".join(table.get(ch, ch) for ch in text)


def split_segments(text: str) -> list[str]:
    return TOKEN_RE.findall(text)


def non_ws_indexes(segments: list[str]) -> list[int]:
    return [idx for idx, seg in enumerate(segments) if not seg.isspace()]


def flip_segment(text: str, token_index: int) -> str:
    segments = split_segments(text)
    indexes = non_ws_indexes(segments)
    if not indexes:
        return text
    if token_index < 0:
        token_index = len(indexes) + token_index
    if token_index < 0 or token_index >= len(indexes):
        return text
    idx = indexes[token_index]
    segments[idx] = convert(segments[idx])
    return "".join(segments)


def flip_each_token(text: str) -> str:
    out = []
    for segment in split_segments(text):
        out.append(segment if segment.isspace() else convert(segment))
    return "".join(out)


def candidate_map(typed: str, current_token: str = "none") -> dict[str, str]:
    candidates = {
        "keep": typed,
        "flip_all": convert(typed),
        "flip_first": flip_segment(typed, 0),
        "flip_last": flip_segment(typed, -1),
        "flip_each": flip_each_token(typed),
    }
    if current_token in {"first", "last"}:
        token_index = 0 if current_token == "first" else -1
        candidates[f"flip_current_{current_token}"] = flip_segment(typed, token_index)

    # Tiny generic repair candidates for duplicate layout-prefix artifacts
    # such as "цwi-fi" or "пgfhf-gfhf". These are candidates only, not
    # production correction rules.
    for name, value in list(candidates.items()):
        repaired = strip_duplicate_layout_prefix(value)
        if repaired != value:
            candidates[f"{name}+strip_prefix"] = repaired
            candidates[f"{name}+strip_prefix+flip_first"] = flip_segment(repaired, 0)
            candidates[f"{name}+strip_prefix+flip_last"] = flip_segment(repaired, -1)
            candidates[f"{name}+strip_prefix+flip_each"] = flip_each_token(repaired)

    # Stable insertion order and no duplicate candidate texts.
    deduped: dict[str, str] = {}
    seen: set[str] = set()
    for name, value in candidates.items():
        if value not in seen:
            deduped[name] = value
            seen.add(value)
    return deduped


def strip_duplicate_layout_prefix(text: str) -> str:
    segments = split_segments(text)
    for idx, segment in enumerate(segments):
        if segment.isspace() or len(segment) < 2:
            continue
        first = segment[0]
        rest = segment[1:]
        if has_cyrillic(first) and has_latin(rest) and (
            is_ascii_technical_token(rest) or looks_like_ascii_hyphen_tail(rest)
        ):
            segments[idx] = rest
    return "".join(segments)


def is_ascii_technical_token(token: str) -> bool:
    return ascii_technical_score(token) is not None


def ascii_technical_score(token: str) -> float | None:
    if not (
        token.isascii()
        and any(ch.isalpha() for ch in token)
        and all(ch.isalnum() or ch in "-_./@:" for ch in token)
    ):
        return None
    if "://" in token or "@" in token or "/" in token:
        return 1.2
    if "-" in token:
        parts = [part for part in token.lower().split("-") if part]
        if len(parts) >= 2 and all(
            is_known_en_word(part) or (len(part) <= 2 and plausible_word(part, "en"))
            for part in parts
        ):
            return 1.2
        return None
    if "." in token:
        parts = [part for part in token.lower().split(".") if part]
        common_tlds = {"com", "ru", "org", "net", "io", "dev", "pro"}
        if len(parts) >= 2 and parts[-1] in common_tlds:
            return 1.2
        return None
    return None


def looks_like_ascii_hyphen_tail(token: str) -> bool:
    return (
        token.isascii()
        and "-" in token
        and any(ch.isalpha() for ch in token)
        and all(ch.isalnum() or ch in "-_" for ch in token)
    )


def read_cases(path: Path) -> list[Case]:
    with path.open("r", encoding="utf-8", newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        return [
            Case(
                id=row["id"],
                category=row["category"],
                typed=row["typed"],
                expected=row["expected"],
                note=row.get("note", ""),
                current_token=row.get("current_token") or "none",
            )
            for row in reader
        ]


def choose_oracle(case: Case) -> tuple[str, str]:
    candidates = candidate_map(case.typed, case.current_token)
    preferred_names = [
        "keep+strip_prefix",
        "flip_last+strip_prefix",
        "flip_first+strip_prefix",
        "keep+strip_prefix+flip_last",
        "keep+strip_prefix+flip_first",
        "keep+strip_prefix+flip_each",
        "flip_last",
        "flip_first",
        "flip_each",
        "flip_all",
        "keep",
    ]
    for name in preferred_names:
        if candidates.get(name) == case.expected:
            return candidates[name], name
    return candidates["keep"], "keep"


def choose_layout_rules(case: Case) -> tuple[str, str]:
    best = ranked_layout_candidates(case)[0]
    return best.text, f"{best.name} score={best.layout_score:.2f} changed={best.changed}"


def ranked_layout_candidates(case: Case) -> list[CandidateScore]:
    scored = [
        CandidateScore(
            name=name,
            text=text,
            layout_score=score_candidate_text(text) + current_token_bonus(case, text),
            changed=changed_token_count(case.typed, text),
            order=idx,
        )
        for idx, (name, text) in enumerate(candidate_map(case.typed, case.current_token).items())
    ]
    return sorted(scored, key=lambda item: (item.layout_score, -item.changed, -item.order), reverse=True)


def current_token_bonus(case: Case, candidate_text: str) -> float:
    if case.current_token not in {"first", "last"}:
        return 0.0

    original = token_at(case.typed, 0 if case.current_token == "first" else -1)
    candidate = token_at(candidate_text, 0 if case.current_token == "first" else -1)
    if original is None or candidate is None:
        return 0.0
    return 1.0 if candidate == convert(original) else -1.0


def token_at(text: str, token_index: int) -> str | None:
    tokens = [segment for segment in split_segments(text) if not segment.isspace()]
    if not tokens:
        return None
    if token_index < 0:
        token_index = len(tokens) + token_index
    if token_index < 0 or token_index >= len(tokens):
        return None
    return tokens[token_index]


def ambiguous_layout_candidates(case: Case, margin: float) -> tuple[CandidateScore, list[CandidateScore]]:
    ranked = ranked_layout_candidates(case)
    best = ranked[0]
    ambiguous = [
        candidate
        for candidate in ranked
        if best.layout_score - candidate.layout_score <= margin
    ]
    return best, ambiguous


def changed_token_count(left: str, right: str) -> int:
    left_tokens = [segment for segment in split_segments(left) if not segment.isspace()]
    right_tokens = [segment for segment in split_segments(right) if not segment.isspace()]
    total = abs(len(left_tokens) - len(right_tokens))
    total += sum(1 for a, b in zip(left_tokens, right_tokens) if a != b)
    return total


def score_candidate_text(text: str) -> float:
    score = 0.0
    tokens = [segment for segment in split_segments(text) if not segment.isspace()]
    if not tokens:
        return -100.0
    for token in tokens:
        score += score_token(token)
    return score / len(tokens)


def score_token(token: str) -> float:
    core = normalize_word(token)
    if not core:
        return 0.0

    tech_score = ascii_technical_score(core)
    if tech_score is not None:
        return tech_score

    if is_protected_ascii_token(core):
        return 1.2

    cyr = has_cyrillic(core)
    lat = has_latin(core)
    if cyr and lat:
        return -1.4

    if lat:
        if len(core) <= 2:
            return 0.2 if plausible_word(core, "en") else -0.9
        if is_known_en_word(core):
            return 1.0
        return 0.2 if plausible_word(core, "en") else -0.9

    if cyr:
        if is_known_ru_word(core):
            return 1.0
        return 0.2 if plausible_word(core, "ru") else -0.9

    return 0.0


def normalize_word(token: str) -> str:
    return token.strip(" \t\r\n.,!?;:\"'()[]{}<>").lower()


def is_protected_ascii_token(token: str) -> bool:
    if not token.isascii() or not any(ch.isalpha() for ch in token):
        return False
    letters = [ch for ch in token if ch.isalpha()]
    if 2 <= len(letters) <= 6 and all(ch.isupper() for ch in letters):
        return True
    has_lower = any(ch.islower() for ch in letters)
    has_upper = any(ch.isupper() for ch in letters)
    return has_lower and has_upper


def is_known_ru_word(word: str) -> bool:
    if word in ru_words():
        return True
    parts = [part for part in word.split("-") if part]
    if len(parts) > 1 and all(part in ru_words() for part in parts):
        return True
    return False


def is_known_en_word(word: str) -> bool:
    if word in en_words():
        return True
    parts = [part for part in word.split("-") if part]
    if len(parts) > 1 and all(part in en_words() for part in parts):
        return True
    return False


def ru_words() -> set[str]:
    if not hasattr(ru_words, "_cache"):
        words = load_hunspell_words(Path("/usr/share/hunspell/ru_RU.dic"), "ru")
        words.update(
            {
                "в",
                "и",
                "к",
                "с",
                "у",
                "о",
                "я",
                "не",
                "да",
                "нет",
                "ну",
                "же",
                "ли",
                "бы",
                "пара",
                "прошу",
                "нужно",
            }
        )
        setattr(ru_words, "_cache", words)
    return getattr(ru_words, "_cache")


def en_words() -> set[str]:
    if not hasattr(en_words, "_cache"):
        words = load_hunspell_words(Path("/usr/share/hunspell/en_US.dic"), "en")
        if not words:
            words = load_plain_words(Path("/usr/share/dict/words"), "en")
        words.update(
            {
                "api",
                "bitnet",
                "dbus",
                "github",
                "gnome",
                "json",
                "llm",
                "qwen",
                "readme",
                "smollm",
                "uinput",
                "usb",
                "wayland",
            }
        )
        setattr(en_words, "_cache", words)
    return getattr(en_words, "_cache")


def load_hunspell_words(path: Path, lang: str) -> set[str]:
    if not path.exists():
        return set()
    out: set[str] = set()
    with path.open("r", encoding="utf-8", errors="ignore") as f:
        for idx, line in enumerate(f):
            if idx == 0:
                continue
            word = normalize_word(line.split("/", 1)[0])
            if word and word_matches_lang(word, lang):
                out.add(word)
    return out


def load_plain_words(path: Path, lang: str) -> set[str]:
    if not path.exists():
        return set()
    out: set[str] = set()
    with path.open("r", encoding="utf-8", errors="ignore") as f:
        for line in f:
            word = normalize_word(line)
            if word and word_matches_lang(word, lang):
                out.add(word)
    return out


def word_matches_lang(word: str, lang: str) -> bool:
    if lang == "ru":
        return all("а" <= ch <= "я" or ch == "ё" or ch == "-" for ch in word)
    return all((ch.isascii() and ch.isalpha()) or ch == "-" for ch in word)


def plausible_word(word: str, lang: str) -> bool:
    letters = [ch for ch in word if ch.isalpha()]
    if len(letters) <= 2:
        return True
    vowels = "аеёиоуыэюя" if lang == "ru" else "aeiou"
    if sum(1 for ch in letters if ch.lower() in vowels) == 0:
        return False
    streak = 0
    for ch in letters:
        if ch.lower() in vowels:
            streak = 0
        else:
            streak += 1
            if streak >= 4:
                return False
    return True


def run_static_layer(cases: list[Case], name: str, chooser: Callable[[Case], tuple[str, str]]) -> list[Decision]:
    out = []
    for case in cases:
        start = time.perf_counter()
        output, detail = chooser(case)
        ms = (time.perf_counter() - start) * 1000
        out.append(Decision(name, case, output, output == case.expected, ms, detail))
    return out


class SageCorrector:
    def __init__(self, model_name: str, threads: int) -> None:
        import torch
        from transformers import AutoModelForSeq2SeqLM, AutoTokenizer

        torch.set_num_threads(threads)
        self.torch = torch
        self.tokenizer = AutoTokenizer.from_pretrained(model_name)
        self.model = AutoModelForSeq2SeqLM.from_pretrained(model_name)
        self.model.eval()

    def __call__(self, case: Case) -> tuple[str, str]:
        inputs = self.tokenizer(case.typed, return_tensors="pt", padding=True, truncation=False)
        max_length = max(8, math.ceil(inputs["input_ids"].shape[1] * 1.7))
        with self.torch.no_grad():
            output_ids = self.model.generate(**inputs, max_length=max_length, num_beams=1, do_sample=False)
        output = self.tokenizer.batch_decode(output_ids, skip_special_tokens=True)[0]
        return output, "generate"


class Tiny2PseudoLikelihoodRanker:
    def __init__(self, model_name: str, threads: int) -> None:
        import torch
        from transformers import AutoModelForMaskedLM, AutoTokenizer

        torch.set_num_threads(threads)
        self.torch = torch
        self.tokenizer = AutoTokenizer.from_pretrained(model_name)
        self.model = AutoModelForMaskedLM.from_pretrained(model_name)
        self.model.eval()
        if self.tokenizer.mask_token_id is None:
            raise RuntimeError(f"{model_name} has no mask token")

    def score(self, text: str) -> float:
        encoded = self.tokenizer(text, return_tensors="pt", truncation=True, max_length=64)
        input_ids = encoded["input_ids"]
        attention_mask = encoded["attention_mask"]
        token_ids = input_ids[0].tolist()
        maskable_positions = [
            idx
            for idx, token_id in enumerate(token_ids)
            if token_id not in set(self.tokenizer.all_special_ids)
        ]
        if not maskable_positions:
            return float("-inf")

        batch_ids = input_ids.repeat(len(maskable_positions), 1)
        batch_mask = attention_mask.repeat(len(maskable_positions), 1)
        target_ids = []
        for row, pos in enumerate(maskable_positions):
            target_ids.append(int(batch_ids[row, pos]))
            batch_ids[row, pos] = self.tokenizer.mask_token_id

        with self.torch.no_grad():
            logits = self.model(input_ids=batch_ids, attention_mask=batch_mask).logits
            log_probs = self.torch.nn.functional.log_softmax(logits, dim=-1)
            values = [
                float(log_probs[row, pos, target_id])
                for row, (pos, target_id) in enumerate(zip(maskable_positions, target_ids))
            ]
        return statistics.fmean(values)

    def __call__(self, case: Case) -> tuple[str, str]:
        candidates = candidate_map(case.typed, case.current_token)
        scored = [(name, text, self.score(text)) for name, text in candidates.items()]
        name, text, score = max(scored, key=lambda item: item[2])
        detail = f"{name} score={score:.3f}"
        return text, detail


class Tiny2GuardedArbiter:
    def __init__(self, model_name: str, threads: int, margin: float) -> None:
        self.rank_model = Tiny2PseudoLikelihoodRanker(model_name, threads)
        self.margin = margin

    def __call__(self, case: Case) -> tuple[str, str]:
        best, candidates = ambiguous_layout_candidates(case, self.margin)
        if len(candidates) == 1:
            return best.text, f"layout:{best.name} score={best.layout_score:.2f}"

        scored = [
            (candidate, self.rank_model.score(candidate.text))
            for candidate in candidates
        ]
        winner, model_score = max(scored, key=lambda item: (item[1], item[0].layout_score, -item[0].changed))
        return (
            winner.text,
            f"tiny2:{winner.name} model={model_score:.3f} layout={winner.layout_score:.2f} n={len(candidates)}",
        )


class SageStabilityArbiter:
    def __init__(self, model_name: str, threads: int, margin: float) -> None:
        self.corrector = SageCorrector(model_name, threads)
        self.margin = margin

    def __call__(self, case: Case) -> tuple[str, str]:
        best, candidates = ambiguous_layout_candidates(case, self.margin)
        if len(candidates) == 1:
            return best.text, f"layout:{best.name} score={best.layout_score:.2f}"

        scored = []
        for candidate in candidates:
            normalized, _ = self.corrector(
                Case(case.id, case.category, candidate.text, case.expected, case.note, case.current_token)
            )
            stability = normalized_similarity(candidate.text, normalized)
            scored.append((candidate, stability, normalized))

        winner, stability, normalized = max(
            scored,
            key=lambda item: (item[1], item[0].layout_score, -item[0].changed),
        )
        return (
            winner.text,
            f"sage:{winner.name} stability={stability:.3f} normalized={normalized!r} n={len(candidates)}",
        )


class ConsensusArbiter:
    def __init__(self, tiny2_model: str, sage_model: str, threads: int, margin: float) -> None:
        self.tiny2 = Tiny2GuardedArbiter(tiny2_model, threads, margin)
        self.sage = SageStabilityArbiter(sage_model, threads, margin)
        self.margin = margin

    def __call__(self, case: Case) -> tuple[str, str]:
        layout_best, candidates = ambiguous_layout_candidates(case, self.margin)
        if len(candidates) == 1:
            return layout_best.text, f"layout:{layout_best.name} score={layout_best.layout_score:.2f}"

        tiny_text, tiny_detail = self.tiny2(case)
        sage_text, sage_detail = self.sage(case)
        if tiny_text == sage_text:
            return tiny_text, f"consensus model_agree tiny2=({tiny_detail}) sage=({sage_detail})"
        return layout_best.text, f"consensus fallback layout:{layout_best.name}; tiny2={tiny_text!r}; sage={sage_text!r}"


def normalized_similarity(left: str, right: str) -> float:
    left_norm = normalize_for_model_compare(left)
    right_norm = normalize_for_model_compare(right)
    if left_norm == right_norm:
        return 1.0
    max_len = max(len(left_norm), len(right_norm), 1)
    return 1.0 - (edit_distance(left_norm, right_norm) / max_len)


def normalize_for_model_compare(text: str) -> str:
    text = text.lower().replace("ё", "е")
    text = re.sub(r"[^\w\s-]+", "", text, flags=re.UNICODE)
    return " ".join(text.split())


def edit_distance(left: str, right: str) -> int:
    if len(left) < len(right):
        left, right = right, left
    previous = list(range(len(right) + 1))
    for i, left_ch in enumerate(left, start=1):
        current = [i]
        for j, right_ch in enumerate(right, start=1):
            current.append(
                min(
                    previous[j] + 1,
                    current[j - 1] + 1,
                    previous[j - 1] + (left_ch != right_ch),
                )
            )
        previous = current
    return previous[-1]


def run_model_layer(cases: list[Case], name: str, chooser: Callable[[Case], tuple[str, str]]) -> list[Decision]:
    out = []
    for idx, case in enumerate(cases, start=1):
        start = time.perf_counter()
        output, detail = chooser(case)
        ms = (time.perf_counter() - start) * 1000
        out.append(Decision(name, case, output, output == case.expected, ms, detail))
        print(
            f"[{name}] {idx:03d}/{len(cases):03d} {case.id} "
            f"{'OK' if output == case.expected else 'BAD'} {ms:.1f}ms",
            flush=True,
        )
    return out


def summarize(decisions: list[Decision]) -> dict[str, str | float | int]:
    total = len(decisions)
    ok = sum(1 for d in decisions if d.ok)
    timings = [d.ms for d in decisions]
    return {
        "layer": decisions[0].layer if decisions else "-",
        "ok": ok,
        "total": total,
        "accuracy": (ok / total * 100) if total else 0.0,
        "mean_ms": statistics.fmean(timings) if timings else 0.0,
        "p95_ms": percentile(timings, 0.95) if timings else 0.0,
    }


def percentile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    values = sorted(values)
    idx = min(len(values) - 1, math.ceil(len(values) * q) - 1)
    return values[idx]


def markdown_table(headers: list[str], rows: Iterable[Iterable[str]]) -> str:
    rows = [list(row) for row in rows]
    widths = [
        max(len(headers[col]), *(len(row[col]) for row in rows)) if rows else len(headers[col])
        for col in range(len(headers))
    ]
    out = []
    out.append("| " + " | ".join(headers[col].ljust(widths[col]) for col in range(len(headers))) + " |")
    out.append("|-" + "-|-".join("-" * widths[col] for col in range(len(headers))) + "-|")
    for row in rows:
        out.append("| " + " | ".join(row[col].ljust(widths[col]) for col in range(len(headers))) + " |")
    return "\n".join(out)


def render_report(cases: list[Case], decisions_by_layer: list[list[Decision]]) -> str:
    lines = ["# Two-word model evaluation", ""]
    lines.append(f"Cases: {len(cases)}")
    lines.append("")
    summary_rows = []
    for decisions in decisions_by_layer:
        s = summarize(decisions)
        summary_rows.append(
            [
                str(s["layer"]),
                f"{s['ok']}/{s['total']}",
                f"{s['accuracy']:.1f}%",
                f"{s['mean_ms']:.1f}",
                f"{s['p95_ms']:.1f}",
            ]
        )
    lines.append(markdown_table(["Layer", "OK", "Accuracy", "Mean ms", "p95 ms"], summary_rows))
    lines.append("")

    for decisions in decisions_by_layer:
        bad = [d for d in decisions if not d.ok]
        lines.append(f"## {decisions[0].layer}")
        lines.append("")
        lines.append(f"Bad cases: {len(bad)}")
        lines.append("")
        rows = [
            [d.case.id, d.case.category, d.case.typed, d.case.expected, d.output, d.detail]
            for d in bad[:80]
        ]
        if rows:
            lines.append(markdown_table(["ID", "Category", "Typed", "Expected", "Output", "Detail"], rows))
        else:
            lines.append("No bad cases.")
        lines.append("")
    return "\n".join(lines)


def validate_candidate_coverage(cases: list[Case]) -> list[Decision]:
    decisions = []
    for case in cases:
        start = time.perf_counter()
        candidates = candidate_map(case.typed, case.current_token)
        winner = next((name for name, text in candidates.items() if text == case.expected), "")
        output = case.expected if winner else "<missing>"
        ms = (time.perf_counter() - start) * 1000
        decisions.append(Decision("candidate_coverage", case, output, bool(winner), ms, winner or "missing"))
    return decisions


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cases", type=Path, default=Path("eval/two_word_cases.tsv"))
    parser.add_argument("--layers", default="coverage,oracle,layout_rules")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--arbiter-margin", type=float, default=0.05)
    parser.add_argument("--sage-model", default="ai-forever/sage-fredt5-distilled-95m")
    parser.add_argument("--tiny2-model", default="cointegrated/rubert-tiny2")
    parser.add_argument("--report", type=Path, default=Path("eval/two_word_model_eval.md"))
    args = parser.parse_args()

    cases = read_cases(args.cases)
    if args.limit:
        cases = cases[: args.limit]

    selected = [layer.strip() for layer in args.layers.split(",") if layer.strip()]
    decisions_by_layer: list[list[Decision]] = []
    if any(layer in {"coverage", "oracle", "layout_rules", "tiny2_arbiter", "sage_arbiter", "consensus_arbiter"} for layer in selected):
        ru_words()
        en_words()

    for layer in selected:
        if layer == "coverage":
            decisions_by_layer.append(validate_candidate_coverage(cases))
        elif layer == "oracle":
            decisions_by_layer.append(run_static_layer(cases, "candidate_oracle", choose_oracle))
        elif layer == "layout_rules":
            decisions_by_layer.append(run_static_layer(cases, "layout_rules", choose_layout_rules))
        elif layer == "sage":
            decisions_by_layer.append(run_model_layer(cases, "sage", SageCorrector(args.sage_model, args.threads)))
        elif layer == "tiny2":
            decisions_by_layer.append(
                run_model_layer(cases, "tiny2_pseudoll", Tiny2PseudoLikelihoodRanker(args.tiny2_model, args.threads))
            )
        elif layer == "tiny2_arbiter":
            decisions_by_layer.append(
                run_model_layer(
                    cases,
                    "tiny2_arbiter",
                    Tiny2GuardedArbiter(args.tiny2_model, args.threads, args.arbiter_margin),
                )
            )
        elif layer == "sage_arbiter":
            decisions_by_layer.append(
                run_model_layer(
                    cases,
                    "sage_arbiter",
                    SageStabilityArbiter(args.sage_model, args.threads, args.arbiter_margin),
                )
            )
        elif layer == "consensus_arbiter":
            decisions_by_layer.append(
                run_model_layer(
                    cases,
                    "consensus_arbiter",
                    ConsensusArbiter(args.tiny2_model, args.sage_model, args.threads, args.arbiter_margin),
                )
            )
        else:
            raise SystemExit(f"unknown layer: {layer}")

    report = render_report(cases, decisions_by_layer)
    args.report.write_text(report + "\n", encoding="utf-8")
    print(report)


if __name__ == "__main__":
    main()
