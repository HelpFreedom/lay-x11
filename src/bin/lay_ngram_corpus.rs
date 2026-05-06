//! Build and check local n-gram corpora.
//!
//! The generated corpus is intentionally not committed. It is a reproducible
//! local training/stress corpus for the lightweight char n-gram scorer.

use clap::{Parser, Subcommand};
use lay::ngram::{self, CharNgramModel, Lang};
use serde_json::Value;
use std::io::Write;
use std::time::Instant;

const RU_HUNSPELL: &str = "/usr/share/hunspell/ru_RU.dic";
const PROTECTED_WORDS_PATH: &str = ".config/lay/protected_words.txt";
const LEARN_LOG_PATH: &str = ".local/share/lay/corrections.jsonl";

#[derive(Parser, Debug)]
#[command(name = "lay-ngram-corpus")]
#[command(about = "Build/check local char n-gram corpora for lay")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate a local corpus file. Default is 50 MiB.
    Build {
        #[arg(long, default_value = "corpus/ru_50mb.txt")]
        out: std::path::PathBuf,
        #[arg(long, default_value_t = 50)]
        size_mb: u64,
    },
    /// Train a char n-gram model from a corpus and run control pairs.
    Check {
        #[arg(long, default_value = "corpus/ru_50mb.txt")]
        corpus: std::path::PathBuf,
    },
    /// Build a reusable RU n-gram cache. Uses corpus if provided.
    Cache {
        #[arg(long)]
        corpus: Option<std::path::PathBuf>,
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Load the reusable RU n-gram cache and run control pairs.
    CheckCache {
        #[arg(long)]
        cache: Option<std::path::PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    match args.command {
        Command::Build { out, size_mb } => build_corpus(&out, size_mb)?,
        Command::Check { corpus } => check_corpus(&corpus)?,
        Command::Cache { corpus, out } => build_cache(corpus.as_deref(), out.as_deref())?,
        Command::CheckCache { cache } => check_cache(cache.as_deref())?,
    }
    Ok(())
}

fn build_corpus(out: &std::path::Path, size_mb: u64) -> std::io::Result<()> {
    let mut words = Vec::new();
    words.extend(load_hunspell_words(RU_HUNSPELL));

    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        words.extend(load_plain_words(&home.join(PROTECTED_WORDS_PATH)).unwrap_or_default());
        words.extend(load_correction_targets(&home.join(LEARN_LOG_PATH)).unwrap_or_default());
    }

    words.sort();
    words.dedup();
    if words.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no source words for corpus",
        ));
    }

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(out)?;
    let target_bytes = size_mb.saturating_mul(1024 * 1024);
    let mut written = 0_u64;
    let mut index = 0_usize;

    while written < target_bytes {
        let mut line = String::new();
        for _ in 0..12 {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&words[index % words.len()]);
            index += 1;
        }
        line.push('\n');
        file.write_all(line.as_bytes())?;
        written += line.len() as u64;
    }

    println!(
        "built {} ({} bytes, {} unique source words)",
        out.display(),
        written,
        words.len()
    );
    Ok(())
}

fn check_corpus(corpus: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let text = std::fs::read_to_string(corpus)?;
    let read_ms = started.elapsed().as_millis();

    let train_started = Instant::now();
    let model = CharNgramModel::train_from_text(Lang::Ru, &text);
    let train_ms = train_started.elapsed().as_millis();

    println!(
        "corpus={} bytes={} read_ms={} train_ms={}",
        corpus.display(),
        text.len(),
        read_ms,
        train_ms
    );

    check_model(&model)
}

fn build_cache(
    corpus: Option<&std::path::Path>,
    out: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let out = match out {
        Some(path) => path.to_path_buf(),
        None => ngram::default_ru_cache_path().ok_or("HOME is not set")?,
    };

    let started = Instant::now();
    let model = if let Some(corpus) = corpus {
        let text = std::fs::read_to_string(corpus)?;
        CharNgramModel::train_from_text(Lang::Ru, &text)
    } else {
        ngram::build_ru_model_from_sources()
    };
    let train_ms = started.elapsed().as_millis();

    let save_started = Instant::now();
    let bytes = ngram::save_ru_cache(&out, &model)?;
    let save_ms = save_started.elapsed().as_millis();
    println!(
        "cache={} bytes={} train_ms={} save_ms={}",
        out.display(),
        bytes,
        train_ms,
        save_ms
    );
    check_model(&model)
}

fn check_cache(cache: Option<&std::path::Path>) -> Result<(), Box<dyn std::error::Error>> {
    let cache = match cache {
        Some(path) => path.to_path_buf(),
        None => ngram::default_ru_cache_path().ok_or("HOME is not set")?,
    };
    let started = Instant::now();
    let model = ngram::load_ru_cache(&cache)?;
    let load_ms = started.elapsed().as_millis();
    let bytes = std::fs::metadata(&cache)?.len();
    println!(
        "cache={} bytes={} load_ms={}",
        cache.display(),
        bytes,
        load_ms
    );
    check_model(&model)
}

fn check_model(model: &CharNgramModel) -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        ("работает", "рабоатет"),
        ("ладно", "ландо"),
        ("явно", "я вно"),
        ("плохо", "плозо"),
        ("правильно", "првильно"),
    ];

    let mut failed = false;
    for (good, bad) in cases {
        let margin = model.margin(good, bad);
        let ok = margin > 0.0;
        println!(
            "{} > {} margin={:.4} {}",
            good,
            bad,
            margin,
            if ok { "OK" } else { "BAD" }
        );
        failed |= !ok;
    }

    if failed {
        Err("ngram corpus check failed".into())
    } else {
        Ok(())
    }
}

fn load_hunspell_words(path: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| normalize_ru_word(line.split('/').next().unwrap_or("")))
        .collect()
}

fn load_plain_words(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(normalize_ru_word)
        .collect())
}

fn load_correction_targets(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(to) = value.get("to").and_then(Value::as_str) else {
            continue;
        };
        for word in to.split(|ch: char| !ch.is_alphabetic() && ch != '-') {
            if let Some(word) = normalize_ru_word(word) {
                out.push(word);
            }
        }
    }
    Ok(out)
}

fn normalize_ru_word(word: &str) -> Option<String> {
    let word = word.trim().to_lowercase();
    if word.is_empty() || word.chars().count() < 2 {
        return None;
    }
    if !word.chars().all(|ch| matches!(ch, 'а'..='я' | 'ё' | '-')) {
        return None;
    }
    Some(word)
}
