# Local n-gram corpora

Large corpus files are generated locally and ignored by git.

Build a 50 MiB Russian corpus:

```bash
cargo run --bin lay-ngram-corpus -- build --size-mb 50 --out corpus/ru_50mb.txt
```

Train/check the char n-gram scorer on it:

```bash
cargo run --bin lay-ngram-corpus -- check --corpus corpus/ru_50mb.txt
```

Build the reusable runtime cache:

```bash
cargo run --bin lay-ngram-corpus -- cache --corpus corpus/ru_50mb.txt
```

Check runtime cache load speed:

```bash
cargo run --bin lay-ngram-corpus -- check-cache
```

By default the generated corpus is local-only and ignored by git. The builder
can optionally mix Hunspell words with private user sources such as
`~/.config/lay/protected_words.txt` and
`~/.local/share/lay/corrections.jsonl`; do not publish generated corpus files
that were built from personal corrections.
