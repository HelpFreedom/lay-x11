# Two-word model evaluation

This folder is a lab bench for `lay` two-word correction. It does not change the
daemon hot path.

Main table:

```text
eval/two_word_cases.tsv
```

The table is generated and currently contains 3118 fixed cases:

- normal RU/EN text that must stay unchanged;
- English left word plus Russian target typed in the US layout;
- Russian left word plus English target typed in the RU layout;
- both words typed in the wrong layout;
- mixed technical tokens such as `wi-fi`;
- unfinished fragments where manual double-Shift must flip the fragment as-is.

Each row has a `current_token` field:

- `none`: both words are completed context and the scorer should choose safely;
- `last`: the last token is the active typed tail and must be flipped;
- `first`: the first token is the active typed tail after caret movement.

Regenerate the stress table:

```bash
python3.12 scripts/generate_two_word_cases.py
```

Run deterministic candidate coverage and local rules:

```bash
python3.12 scripts/eval_two_word_models.py \
  --layers coverage,oracle,layout_rules
```

Run `rubert-tiny2` as a raw masked-LM scorer:

```bash
/home/ubu/.cache/lay-model-eval/venv/bin/python \
  scripts/eval_two_word_models.py \
  --layers tiny2 \
  --threads 4 \
  --report eval/two_word_model_eval_tiny2.md
```

Run `sage-fredt5-distilled-95m` as a raw generator:

```bash
/home/ubu/.cache/lay-model-eval/venv/bin/python \
  scripts/eval_two_word_models.py \
  --layers sage \
  --threads 4 \
  --report eval/two_word_model_eval_sage.md
```

Current result on the generated stress table:

- candidate coverage: 3118/3118;
- deterministic `layout_rules`: 3118/3118;
- raw `rubert-tiny2` on the first 20 mixed cases: 0/20;
- raw `sage-fredt5-distilled-95m` on the first 20 mixed cases: 0/20;
- guarded `rubert-tiny2` arbiter: 3108/3118;
- guarded `sage-fredt5` arbiter: 3115/3118;
- guarded consensus arbiter: 3115/3118.

Conclusion: neither model should replace `lay` candidate generation. For this
task, `lay` must build the variants first. Models can only be tested as cautious
arbiters or normalizers around those variants. On the current two-word stress
table, even guarded model arbiters make the result worse than deterministic
rules, mostly around protected brand + single-letter cases.

## Runtime Smoke

The model matrix is text-only. For the live double-Shift contract, use:

```bash
scripts/run_runtime_smoke.py
```

This opens a real Zenity/GTK entry field, creates a virtual keyboard through
`lay-test-input`, starts a temporary `lay-daemon --device /dev/input/eventX`,
sends physical key events, presses double-Shift, and compares the text returned
by the entry after Enter.

Current smoke cases:

- `ghbdtn_enter` -> `привет`;
- `dhtvz_toggle_enter` -> `dhtvz`;
- `good_ntrcn_enter` -> `good текст`;
- `good_text_enter` -> `good текст`;
- `good_vshgidu_enter` -> `good Double`;
- `mixed_word` -> `при`;
- `n_teper_mixed_enter` -> `Теперь`;
- `proverka_ntrcn_enter` -> `проверка текст`;
- `vyvodim_dva_enter` -> `выводим два`;
- `wifi_ye_enter` -> `wi-fi ну`.
