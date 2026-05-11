//! lay — Caramba/Punto-style конвертер раскладки клавиатуры.
//!
//! Детерминированная конвертация US ↔ RU за микросекунды.

use clap::Parser;
use std::io::{self, IsTerminal, Read};
use std::process;

use lay::dict;

#[derive(Parser, Debug)]
#[command(
    name = "lay",
    version,
    about = "Layout switcher: 'Ye djn ghbvth' → 'Ну вот пример'"
)]
struct Args {
    /// Текст для конвертации (если пусто — читаем stdin или --clipboard).
    text: Vec<String>,

    /// Читать из/писать в буфер обмена.
    #[arg(short, long)]
    clipboard: bool,

    /// Печатать какой метод сработал.
    #[arg(short, long)]
    verbose: bool,
}

fn main() {
    let args = Args::parse();

    let text = if args.clipboard {
        match read_clipboard() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("⚠ не удалось прочитать буфер: {e}");
                process::exit(1);
            }
        }
    } else if !args.text.is_empty() {
        args.text.join(" ")
    } else if io::stdin().is_terminal() {
        eprintln!("Использование: lay <текст>  |  lay --clipboard  |  echo '...' | lay");
        process::exit(1);
    } else {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).ok();
        s
    };

    if text.trim().is_empty() {
        process::exit(0);
    }

    let direction = dict::detect_direction(&text);
    let result = dict::convert(&text, direction);

    if args.clipboard {
        if let Err(e) = write_clipboard(&result) {
            eprintln!("⚠ не удалось записать в буфер: {e}");
            process::exit(1);
        }
        if args.verbose {
            eprintln!("[dict] {text:?} → {result:?}");
        } else {
            eprintln!("✓ в буфере обмена");
        }
    } else {
        if args.verbose {
            eprintln!("[dict]");
        }
        print!("{result}");
        if text.ends_with('\n') && !result.ends_with('\n') {
            println!();
        }
    }
}

fn read_clipboard() -> Result<String, Box<dyn std::error::Error>> {
    let mut cb = arboard::Clipboard::new()?;
    Ok(cb.get_text()?)
}

fn write_clipboard(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_string())?;
    Ok(())
}
