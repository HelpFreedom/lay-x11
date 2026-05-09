//! lay-daemon — Caramba/Punto-style для GNOME Wayland.
//!
//! Базовый replay-принцип: запоминаем физические нажатия клавиш и при двойном
//! Shift:
//!   1) стираем последнее слово через uinput Backspace × N,
//!   2) переключаем раскладку через GNOME Shell extension,
//!   3) повторяем те же физические клавиши через uinput — gnome-shell сам
//!      интерпретирует их в новой раскладке.
//!
//! Этот replay core не требует словарной конвертации. Smart/typing-assist
//! ветки дополнительно используют RU/EN-таблицы, словари и n-gram scorer; они
//! сейчас оптимизированы и протестированы именно для RU/EN.

use clap::Parser;
use evdev::{uinput::VirtualDevice, AttributeSet, Device, EventType, InputEvent, KeyCode};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONFIG_PATH: &str = ".config/lay/config.json"; // относительно $HOME
const REPLACEMENTS_PATH: &str = ".config/lay/replacements.json"; // относительно $HOME
const PROTECTED_WORDS_PATH: &str = ".config/lay/protected_words.txt"; // относительно $HOME
const LEARN_LOG_PATH: &str = ".local/share/lay/corrections.jsonl"; // относительно $HOME
const LEARN_CANDIDATES_PATH: &str = ".local/share/lay/learning_candidates.json"; // относительно $HOME
const LEARN_LOG_MAX_BYTES: u64 = 1024 * 1024;
const LEARN_LOG_KEEP_LINES: usize = 3000;
const LEARN_PROMOTION_THRESHOLD: u32 = 2;
const KEY_PACE_MS: u64 = 1;
const BACKSPACE_DOWN_MS: u64 = 1;
const BACKSPACE_PACE_MS: u64 = 2;
const BACKSPACE_SETTLE_MS: u64 = 16;
const TEXT_REPLACE_KEY_PACE_MS: u64 = 1;
const TEXT_REPLACE_BACKSPACE_DOWN_MS: u64 = 1;
const TEXT_REPLACE_BACKSPACE_PACE_MS: u64 = 1;
const TEXT_REPLACE_BACKSPACE_SETTLE_MS: u64 = 16;
const TEXT_INSERT_KEY_PACE_MS: u64 = 2;
const TEXT_INSERT_SPACE_SETTLE_MS: u64 = 8;
const MODIFIER_RELEASE_ROUNDS: usize = 2;
const MODIFIER_RELEASE_PACE_MS: u64 = 3;
const LAYOUT_POLL_INTERVAL_MS: u64 = 250;
const NGRAM_TYPO_REJECT_MARGIN: f64 = 0.25;
const NGRAM_SPLIT_REJECT_MARGIN: f64 = 0.25;
const NGRAM_NODICT_SPLIT_REJECT_MARGIN: f64 = 1.0;
const NGRAM_DICT_MISSING_LETTER_MARGIN: f64 = -8.0;
const NGRAM_MISSING_LETTER_MARGIN: f64 = 1.5;
const NGRAM_HARD_SIGN_MARGIN: f64 = 1.0;
const NGRAM_MOVED_PREFIX_MARGIN: f64 = 0.5;
const NGRAM_MOVED_PREFIX_RIGHT_MARGIN: f64 = 5.0;
const LEARNING_FEEDBACK_MAX_AGE_SECS: u64 = 30;
const MAX_REPLACE_WORDS: usize = 8;
const RU_ALPHABET: [char; 33] = [
    'а', 'б', 'в', 'г', 'д', 'е', 'ё', 'ж', 'з', 'и', 'й', 'к', 'л', 'м', 'н', 'о', 'п', 'р', 'с',
    'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ъ', 'ы', 'ь', 'э', 'ю', 'я',
];
static DBUS_CONNECTION: OnceLock<Mutex<Option<zbus::blocking::Connection>>> = OnceLock::new();

// ─── Config ─────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
struct LayConfig {
    /// Legacy field: simple | llm. New UI writes correction_engine instead.
    mode: String,
    /// Engine for double-Shift correction: replay | smart.
    correction_engine: Option<String>,
    /// Триггер: double-* | caps-lock | single-*
    trigger: String,
    /// Максимальная длительность каждого тапа (мс)
    tap_max_ms: u64,
    /// Окно между двумя тапами (мс)
    shift_window_ms: u64,
    /// Дебаунс после конвертации (мс)
    debounce_ms: u64,
    /// Сколько последних слов менять: 1..2 независимо от engine.
    replace_words: usize,
    /// Точные персональные автоподмены после обычной перекладки.
    auto_replace: bool,
    /// Безопасная помощь при наборе после пробела: только точные правила.
    typing_assist: bool,
    /// После автоматической помощи при наборе оставлять активной раскладку результата.
    auto_switch_layout: bool,
    /// Локальный opt-in лог исправлений для будущего обучения.
    learning_log: bool,
}

impl Default for LayConfig {
    fn default() -> Self {
        Self {
            mode: "simple".into(),
            correction_engine: None,
            trigger: "double-lshift".into(),
            tap_max_ms: 200,
            shift_window_ms: 250,
            debounce_ms: 50,
            replace_words: 1,
            auto_replace: false,
            typing_assist: false,
            auto_switch_layout: true,
            learning_log: false,
        }
    }
}

impl LayConfig {
    fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{}/{}", home, CONFIG_PATH);
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!("[lay-daemon] config parse error: {e}, using defaults");
                Self::default()
            }),
            Err(_) => Self::default(), // файл не существует — дефолты
        }
    }

    fn active_replace_words(&self) -> usize {
        self.replace_words.clamp(1, 2)
    }

    fn active_correction_engine(&self) -> CorrectionEngine {
        match self.correction_engine.as_deref() {
            Some("smart") => CorrectionEngine::Smart,
            Some("replay") => CorrectionEngine::Replay,
            // Compatibility with configs written before correction_engine existed.
            _ if self.mode == "llm" => CorrectionEngine::Smart,
            _ => CorrectionEngine::Replay,
        }
    }
}

fn active_replace_words() -> usize {
    LayConfig::load().active_replace_words()
}

fn active_correction_engine() -> CorrectionEngine {
    LayConfig::load().active_correction_engine()
}

fn active_auto_replace() -> bool {
    LayConfig::load().auto_replace
}

fn active_typing_assist() -> bool {
    LayConfig::load().typing_assist
}

fn active_auto_switch_layout() -> bool {
    LayConfig::load().auto_switch_layout
}

fn active_learning_log() -> bool {
    LayConfig::load().learning_log
}

const DBUS_PATH: &str = "/io/github/radislabus_star/LayDaemon";
const DBUS_INTERFACE: &str = "io.github.radislabus_star.LayDaemon";
const DBUS_DEST: &str = "org.gnome.Shell";

#[derive(Parser, Debug)]
#[command(
    name = "lay-daemon",
    version,
    about = "Caramba-style daemon for Linux Wayland"
)]
struct Args {
    /// Не вызывать DBus extension и не эмулировать — только лог.
    #[arg(long)]
    detect_only: bool,
    /// Принудительно использовать конкретное устройство клавиатуры.
    #[arg(long)]
    device: Option<String>,
    /// Verbose: лог каждого нажатия в stderr/journal. Может содержать набранный текст.
    #[arg(short, long)]
    verbose: bool,
    /// Писать диагностический вывод в stderr/journal. Может содержать набранный текст.
    #[arg(long)]
    debug_log: bool,
}

#[derive(Clone, Copy, Debug)]
struct KeyEvent {
    keycode: u16,
    shift: bool,
    layout_is_ru: bool,
}

struct ExecutingGuard<'a>(&'a mut bool);

impl Drop for ExecutingGuard<'_> {
    fn drop(&mut self) {
        *self.0 = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorrectionEngine {
    Replay,
    Smart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Correction {
    ReplayAll,
    InsertText(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextReplacement {
    move_left: u32,
    backspaces: u32,
    insert: String,
    move_right: u32,
}

#[derive(Debug, Clone)]
struct TextInputRun {
    target_is_ru: bool,
    events: Vec<KeyEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextInsertMethod {
    UinputReplay,
    TypeTextFallback,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    set_log_enabled(args.debug_log || args.verbose || args.detect_only);

    let device_paths: Vec<std::path::PathBuf> = match args.device.clone() {
        Some(p) => vec![std::path::PathBuf::from(p)],
        None => find_all_keyboards()?,
    };
    log(&format!("► старт, устройства: {device_paths:?}"));
    log(&format!(
        "► режим: {}",
        if args.detect_only {
            "DETECT-ONLY"
        } else {
            "LIVE (DBus + uinput)"
        }
    ));
    let startup_cfg = LayConfig::load();
    if !args.detect_only && startup_cfg.active_correction_engine() == CorrectionEngine::Smart {
        std::thread::spawn(|| match lay::llm::warm_up() {
            Ok(()) => log("► smart engine: модель прогрета заранее"),
            Err(e) => log(&format!("⚠ smart engine warmup failed: {e}")),
        });
    }

    // DBus extension для переключения раскладки и TypeText fallback.
    if !args.detect_only {
        match call_ping() {
            Ok(reply) => {
                log(&format!("► extension: {reply}"));
            }
            Err(e) => {
                log(&format!("⚠ extension не отвечает ({e})"));
                log("⚠ работаю в detect-only");
            }
        }
    }

    // Virtual keyboard через uinput для re-typing физических кнопок
    let virtual_kbd = if args.detect_only {
        None
    } else {
        match make_virtual_keyboard() {
            Ok(d) => {
                log("► uinput virtual keyboard создан");
                Some(d)
            }
            Err(e) => {
                log(&format!(
                    "⚠ uinput недоступен ({e}). Re-typing работать не будет"
                ));
                None
            }
        }
    };

    // Spawn один тред на каждую клавиатуру. Каждый тред держит свой
    // буфер и shift_state — клавиатуры независимы, что корректно
    // (если у пользователя 2 клавиатуры — он печатает на одной).
    use std::sync::{Arc, Mutex};
    let virtual_kbd = Arc::new(Mutex::new(virtual_kbd));

    let mut handles = Vec::new();
    for path in device_paths {
        let virtual_kbd = Arc::clone(&virtual_kbd);
        let v = args.verbose;
        let cfg = LayConfig::load();
        handles.push(std::thread::spawn(move || {
            if let Err(e) = listen_keyboard(path, virtual_kbd, v, cfg) {
                log(&format!("⚠ thread keyboard: {e}"));
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn listen_keyboard(
    device_path: std::path::PathBuf,
    virtual_kbd: std::sync::Arc<std::sync::Mutex<Option<VirtualDevice>>>,
    verbose: bool,
    cfg: LayConfig,
) -> std::io::Result<()> {
    let mut device = Device::open(&device_path)?;
    log(&format!(
        "► слушаю: {device_path:?} имя={:?}",
        device.name().unwrap_or("?")
    ));
    log(&format!(
        "► config: mode={} replace_words={} auto_replace={} typing_assist={} auto_switch_layout={} trigger={} tap={}ms window={}ms debounce={}ms",
        cfg.mode,
        cfg.replace_words,
        cfg.auto_replace,
        cfg.typing_assist,
        cfg.auto_switch_layout,
        cfg.trigger,
        cfg.tap_max_ms,
        cfg.shift_window_ms,
        cfg.debounce_ms
    ));

    // Клавиша-триггер из config
    let trigger_key = match cfg.trigger.as_str() {
        "double-ctrl" => KeyCode::KEY_LEFTCTRL,
        "double-alt" => KeyCode::KEY_LEFTALT,
        "caps-lock" => KeyCode::KEY_CAPSLOCK,
        "single-rshift" => KeyCode::KEY_RIGHTSHIFT,
        "single-rctrl" => KeyCode::KEY_RIGHTCTRL,
        "single-ralt" => KeyCode::KEY_RIGHTALT,
        "single-pause" => KeyCode::KEY_PAUSE,
        _ => KeyCode::KEY_LEFTSHIFT, // default: double-lshift
    };
    let is_caps_trigger = cfg.trigger == "caps-lock";
    // Одиночный триггер: нажал и отпустил без других клавиш — конвертация
    let is_single_trigger = cfg.trigger.starts_with("single-");
    let mut single_pressed_at: Option<Instant> = None; // когда нажата single-клавиша
    let mut single_other_key = false; // была ли другая клавиша пока держали

    let mut buffer = WordBuffer::new();
    let mut shift_state = ShiftState::default();
    let mut dshift_state = DShiftState::Idle;
    let mut executing = false;
    let shift_tap_max = Duration::from_millis(cfg.tap_max_ms);
    let shift_window = Duration::from_millis(cfg.shift_window_ms);
    let debounce_window = Duration::from_millis(cfg.debounce_ms);
    let mut current_layout_is_ru = read_current_layout_is_ru().unwrap_or(false);
    let mut last_layout_poll = Instant::now();
    let mut last_double_at: Option<Instant> = None;
    // После DOUBLE буфер сохраняется (для toggle). Но как только пользователь
    // начнёт печатать НОВОЕ слово — нужно сбросить буфер чтобы новое слово
    // не приклеилось к предыдущему.
    let mut clear_on_next_typing: bool = false;
    // Перекрёстный счёт: считаем ВСЕ typing-events (press+repeat) с момента
    // последнего пробела/границы, независимо от accept-фильтра. На DOUBLE
    // сравниваем с buffer.current.len() — должны совпасть. Если нет —
    // видно где autorepeat терялся.
    let mut events_since_word_start: u32 = 0;

    loop {
        for event in device.fetch_events()? {
            if event.event_type() != EventType::KEY {
                continue;
            }
            let code = event.code();
            let value = event.value();
            let key = KeyCode::new(code);

            // (тайм-аут очистки буфера убран — буфер чистится только на
            // явных границах: Enter/Tab/Esc/стрелки/Backspace/Delete)

            // ─── флаг выполнения: пока идёт замена — все события в игнор ───
            // Clutter virtual device (TypeText fallback) создаёт evdev-устройство
            // которое мы тоже слушаем → feedback loop: TypeText-события попадают
            // обратно в буфер. Блокируем ВСЕ ключи пока executing=true.
            // modifier state обновляем всё равно — чтобы не рассинхронизироваться.
            if executing {
                shift_state.update(key, value);
                continue;
            }

            // ─── modifier tracking ────────────────────────────
            shift_state.update(key, value);

            // ═══ FSM: press→release→press→release = DOUBLE TRIGGER ════
            // RShift и RAlt не участвуют в FSM
            // Single-trigger клавиши не фильтруем (они сами и есть триггер)
            if !is_single_trigger
                && (key == KeyCode::KEY_RIGHTSHIFT || key == KeyCode::KEY_RIGHTALT)
            {
                continue;
            }

            // ─── Single trigger (правый Shift/Ctrl/Alt/Pause) ───────────
            // Нажал без других клавиш → отпустил ≤ tap_max → конвертация
            if is_single_trigger {
                if key == trigger_key {
                    match value {
                        1 => {
                            // press
                            single_pressed_at = Some(Instant::now());
                            single_other_key = false;
                        }
                        0 => {
                            // release
                            if let Some(t) = single_pressed_at.take() {
                                let held = t.elapsed();
                                if !single_other_key
                                    && held <= shift_tap_max
                                    && last_double_at
                                        .map_or(true, |d| d.elapsed() >= debounce_window)
                                {
                                    let buf_count = buffer.current.len() as u32;
                                    log(&format!(
                                        "═ CROSS-CHECK: buffer={} events={}{}",
                                        buf_count,
                                        events_since_word_start,
                                        if buf_count != events_since_word_start {
                                            " ⚠"
                                        } else {
                                            " ✓"
                                        }
                                    ));
                                    let mut g = virtual_kbd.lock().unwrap();
                                    let replace_words = active_replace_words();
                                    let engine = active_correction_engine();
                                    let auto_replace = active_auto_replace();
                                    if let Some(is_ru) = handle_double_shift(
                                        &mut buffer,
                                        replace_words,
                                        engine,
                                        auto_replace,
                                        g.as_mut(),
                                        &mut executing,
                                    ) {
                                        current_layout_is_ru = is_ru;
                                        last_layout_poll = Instant::now();
                                    }
                                    drop(g);
                                    last_double_at = Some(Instant::now());
                                    clear_on_next_typing = true;
                                    log(&format!(
                                        "· single-trigger fired (held {}ms)",
                                        held.as_millis()
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                } else if value == 1 {
                    // Другая клавиша нажата пока держим триггер → отмена
                    single_other_key = true;
                }
                // Для single-trigger продолжаем обычную обработку buffer
            }

            // CapsLock — специальный режим: одно нажатие = триггер
            if is_caps_trigger && key == KeyCode::KEY_CAPSLOCK && value == 1 {
                if let Some(t) = last_double_at {
                    if t.elapsed() < debounce_window {
                        continue;
                    }
                }
                let buf_count = buffer.current.len() as u32;
                log(&format!(
                    "═ CROSS-CHECK: buffer.current={} events={}{}",
                    buf_count,
                    events_since_word_start,
                    if buf_count != events_since_word_start {
                        " ⚠ MISMATCH"
                    } else {
                        " ✓"
                    }
                ));
                let mut g = virtual_kbd.lock().unwrap();
                let replace_words = active_replace_words();
                let engine = active_correction_engine();
                let auto_replace = active_auto_replace();
                if let Some(is_ru) = handle_double_shift(
                    &mut buffer,
                    replace_words,
                    engine,
                    auto_replace,
                    g.as_mut(),
                    &mut executing,
                ) {
                    current_layout_is_ru = is_ru;
                    last_layout_poll = Instant::now();
                }
                drop(g);
                dshift_state = DShiftState::Idle;
                last_double_at = Some(Instant::now());
                clear_on_next_typing = true;
                log("· CAPS LOCK triggered");
                continue;
            }
            if key == trigger_key && !is_caps_trigger {
                // Дебаунс после DOUBLE
                if let Some(t) = last_double_at {
                    if t.elapsed() < debounce_window {
                        continue;
                    }
                }

                let now = Instant::now();
                match (value, dshift_state) {
                    // ── press ──
                    (1, DShiftState::Idle) => {
                        dshift_state = DShiftState::FirstPress { pressed_at: now };
                        if verbose {
                            log("· FSM: Idle → FirstPress");
                        }
                    }
                    (1, DShiftState::WaitingSecond { first_release }) => {
                        if now.duration_since(first_release) <= shift_window {
                            dshift_state = DShiftState::SecondPress { second_press: now };
                            if verbose {
                                log("· FSM: WaitingSecond → SecondPress");
                            }
                        } else {
                            // слишком долго ждали — начинаем сначала
                            dshift_state = DShiftState::FirstPress { pressed_at: now };
                            if verbose {
                                log("· FSM: timeout → FirstPress");
                            }
                        }
                    }
                    (1, _) => {
                        // повторный press без release — игнор (autorepeat Shift)
                    }

                    // ── release ──
                    (0, DShiftState::FirstPress { pressed_at }) => {
                        let held = now.duration_since(pressed_at);
                        if held <= shift_tap_max {
                            dshift_state = DShiftState::WaitingSecond { first_release: now };
                            if verbose {
                                log(&format!(
                                    "· FSM: FirstPress → WaitingSecond (held {}ms)",
                                    held.as_millis()
                                ));
                            }
                        } else {
                            // держали долго — заглавная буква, не двойной Shift
                            dshift_state = DShiftState::Idle;
                            if verbose {
                                log(&format!(
                                    "· FSM: FirstPress → Idle (held {}ms, заглавная)",
                                    held.as_millis()
                                ));
                            }
                        }
                    }
                    (0, DShiftState::SecondPress { second_press, .. }) => {
                        let held = now.duration_since(second_press);
                        if held <= shift_tap_max {
                            // DOUBLE SHIFT! press→release→press→release ✓
                            let buf_count = buffer.current.len() as u32;
                            log(&format!(
                                "═ CROSS-CHECK: buffer.current={} events_since_word_start={}{}",
                                buf_count,
                                events_since_word_start,
                                if buf_count != events_since_word_start {
                                    " ⚠ MISMATCH"
                                } else {
                                    " ✓"
                                }
                            ));
                            let mut g = virtual_kbd.lock().unwrap();
                            let replace_words = active_replace_words();
                            let engine = active_correction_engine();
                            let auto_replace = active_auto_replace();
                            if let Some(is_ru) = handle_double_shift(
                                &mut buffer,
                                replace_words,
                                engine,
                                auto_replace,
                                g.as_mut(),
                                &mut executing,
                            ) {
                                current_layout_is_ru = is_ru;
                                last_layout_poll = Instant::now();
                            }
                            drop(g);
                            dshift_state = DShiftState::Idle;
                            last_double_at = Some(Instant::now());
                            clear_on_next_typing = true;
                            log("· FSM: DOUBLE! (p→r→p→r)");
                        } else {
                            // второй Shift держали долго — не двойной
                            dshift_state = DShiftState::Idle;
                            if verbose {
                                log(&format!(
                                    "· FSM: SecondPress → Idle (held {}ms, не тап)",
                                    held.as_millis()
                                ));
                            }
                        }
                    }
                    (0, _) => {
                        // release в Idle или WaitingSecond — сброс
                        dshift_state = DShiftState::Idle;
                    }
                    _ => {}
                }
                continue;
            }

            // Любая ДРУГАЯ клавиша (press) сбрасывает FSM,
            // НО только до SecondPress — если второй Shift уже нажат,
            // ждём только его release (другие клавиши не мешают).
            if !matches!(
                dshift_state,
                DShiftState::Idle | DShiftState::SecondPress { .. }
            ) && value == 1
            {
                if verbose {
                    log(&format!("· FSM: cancel → Idle (key {code})"));
                }
                dshift_state = DShiftState::Idle;
            }

            // release не интересен — пропускаем
            if value == 0 {
                continue;
            }

            if should_ignore_buffer_key(key, &shift_state, buffer.current.is_empty()) {
                if verbose {
                    log(&format!("· key {code} ignored for buffer (shortcut/noise)"));
                }
                continue;
            }

            // ─── пробел: переносим current → prev (только на press) ──
            if key == KeyCode::KEY_SPACE {
                if value == 1 {
                    if let Some(correction) = buffer.take_user_learning_correction(true) {
                        append_user_correction_learning_log(&correction);
                    }
                    buffer.handle_space();
                    events_since_word_start = 0;
                    if active_typing_assist() {
                        let mut g = virtual_kbd.lock().unwrap();
                        handle_typing_assist_after_space(&mut buffer, g.as_mut(), &mut executing);
                    }
                    if verbose {
                        log(&format!(
                            "· space, history={:?}, current={:?}",
                            buffer.prev_words.len(),
                            buffer.current.len()
                        ));
                    }
                }
                continue;
            }

            // ─── граница (Enter/Tab/Esc/стрелки/BS/Del) — сброс на press ──
            if matches!(key, KeyCode::KEY_BACKSPACE | KeyCode::KEY_DELETE) && value == 1 {
                buffer.note_learning_backspace();
            }
            if is_hard_boundary(key) {
                if value == 1 && !buffer.is_empty() {
                    if !matches!(key, KeyCode::KEY_BACKSPACE | KeyCode::KEY_DELETE) {
                        if let Some(correction) = buffer.take_user_learning_correction(false) {
                            append_user_correction_learning_log(&correction);
                        }
                    }
                    buffer.reset_all();
                    events_since_word_start = 0;
                    if verbose {
                        log(&format!("· reset (граница: {key:?})"));
                    }
                }
                continue;
            }

            // ─── обычный символ ─────
            if is_typing_key(key) {
                if clear_on_next_typing {
                    buffer.reset_all();
                    events_since_word_start = 0;
                    clear_on_next_typing = false;
                }
                let starts_new_word = buffer.current.is_empty();
                // Перекрёстный счёт — увеличиваем НА КАЖДОЕ press/repeat
                // независимо от accept-фильтра.
                events_since_word_start += 1;
                // v=2 (autorepeat) — добавляем ТОЛЬКО если это repeat той же
                // клавиши что была последней. Иначе чужой repeat ломал бы счёт.
                let accept = if value == 2 {
                    matches!(buffer.current.last(), Some(last) if last.keycode == code)
                } else {
                    true
                };
                if !accept {
                    if verbose {
                        log(&format!("· key {code} v=2 SKIP (autorepeat другой) events={events_since_word_start}"));
                    }
                    continue;
                }
                if starts_new_word
                    || last_layout_poll.elapsed() >= Duration::from_millis(LAYOUT_POLL_INTERVAL_MS)
                {
                    if let Ok(is_ru) = read_current_layout_is_ru() {
                        current_layout_is_ru = is_ru;
                    }
                    last_layout_poll = Instant::now();
                }
                buffer.push(KeyEvent {
                    keycode: code,
                    shift: shift_state.any(),
                    layout_is_ru: current_layout_is_ru,
                });
                buffer.note_learning_typed(KeyEvent {
                    keycode: code,
                    shift: shift_state.any(),
                    layout_is_ru: current_layout_is_ru,
                });
                if verbose {
                    log(&format!(
                        "· key {code} v={value} shift={} → current={} events={events_since_word_start}",
                        shift_state.any(),
                        buffer.current.len()
                    ));
                }
            }
        }
    }
}

#[derive(Default)]
struct ShiftState {
    left: bool,
    right: bool,
    left_ctrl: bool,
    right_ctrl: bool,
    left_alt: bool,
    right_alt: bool,
    left_meta: bool,
    right_meta: bool,
}
impl ShiftState {
    fn update(&mut self, key: KeyCode, value: i32) {
        let pressed = value != 0;
        match key {
            KeyCode::KEY_LEFTSHIFT => self.left = pressed,
            KeyCode::KEY_RIGHTSHIFT => self.right = pressed,
            KeyCode::KEY_LEFTCTRL => self.left_ctrl = pressed,
            KeyCode::KEY_RIGHTCTRL => self.right_ctrl = pressed,
            KeyCode::KEY_LEFTALT => self.left_alt = pressed,
            KeyCode::KEY_RIGHTALT => self.right_alt = pressed,
            KeyCode::KEY_LEFTMETA => self.left_meta = pressed,
            KeyCode::KEY_RIGHTMETA => self.right_meta = pressed,
            _ => {}
        }
    }

    fn any(&self) -> bool {
        self.left || self.right
    }

    fn shortcut_active(&self) -> bool {
        self.left_ctrl
            || self.right_ctrl
            || self.left_alt
            || self.right_alt
            || self.left_meta
            || self.right_meta
    }
}

/// FSM для детекции двойного левого Shift по паттерну press→release→press→release.
///
/// Каждый Shift должен быть именно тапом (≤ tap_max мс).
/// Если держать дольше — это заглавная буква, не двойной Shift.
/// Любая другая клавиша в любом состоянии → Idle (отмена).
#[derive(Debug, Clone, Copy)]
enum DShiftState {
    Idle,
    /// Первый Shift нажат, ждём release
    FirstPress {
        pressed_at: Instant,
    },
    /// Первый тап завершён, ждём второй press
    WaitingSecond {
        first_release: Instant,
    },
    /// Второй Shift нажат, ждём release → DOUBLE
    SecondPress {
        second_press: Instant,
    },
}

// ─── WordBuffer ─────────────────────────────────────────────

struct WordBuffer {
    current: Vec<KeyEvent>,
    prev_words: Vec<Vec<KeyEvent>>,
    prev_had_trailing_space: bool,
    replay_toggle_ready: bool,
    pending_learning: Option<PendingLearningCorrection>,
    last_input: Instant,
}

#[derive(Debug, Clone)]
struct PendingLearningCorrection {
    lay_kind: String,
    lay_from: String,
    lay_to: String,
    replace_words: usize,
    words: usize,
    started_at: Instant,
    deleted_chars: u32,
    typed: Vec<KeyEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserLearningCorrection {
    lay_kind: String,
    lay_from: String,
    lay_to: String,
    from: String,
    to: String,
    replace_words: usize,
    words: usize,
}

impl WordBuffer {
    fn new() -> Self {
        Self {
            current: Vec::with_capacity(32),
            prev_words: Vec::with_capacity(MAX_REPLACE_WORDS),
            prev_had_trailing_space: false,
            replay_toggle_ready: false,
            pending_learning: None,
            last_input: Instant::now(),
        }
    }
    fn push(&mut self, e: KeyEvent) {
        self.current.push(e);
        self.prev_had_trailing_space = false;
        self.replay_toggle_ready = false;
        self.last_input = Instant::now();
    }
    fn handle_space(&mut self) {
        if !self.current.is_empty() {
            self.prev_words.push(std::mem::take(&mut self.current));
            if self.prev_words.len() > MAX_REPLACE_WORDS {
                self.prev_words.remove(0);
            }
            self.prev_had_trailing_space = true;
        }
        self.last_input = Instant::now();
    }
    fn reset_all(&mut self) {
        self.current.clear();
        self.prev_words.clear();
        self.prev_had_trailing_space = false;
        self.replay_toggle_ready = false;
        self.last_input = Instant::now();
    }
    fn is_empty(&self) -> bool {
        self.current.is_empty() && self.prev_words.is_empty()
    }

    fn has_current_word(&self) -> bool {
        !self.current.is_empty()
    }

    fn last_completed_words_events(&self, count: usize) -> Option<Vec<KeyEvent>> {
        if !self.prev_had_trailing_space || count == 0 || self.prev_words.len() < count {
            return None;
        }

        let mut events = Vec::new();
        for word in self.prev_words.iter().skip(self.prev_words.len() - count) {
            if !events.is_empty() {
                events.push(KeyEvent {
                    keycode: KeyCode::KEY_SPACE.code(),
                    shift: false,
                    layout_is_ru: false,
                });
            }
            events.extend(word.iter().copied());
        }
        events.push(KeyEvent {
            keycode: KeyCode::KEY_SPACE.code(),
            shift: false,
            layout_is_ru: false,
        });
        Some(events)
    }

    fn mark_replayed_layout(&mut self, replace_words: usize, layout_is_ru: bool) {
        let replace_words = replace_words.clamp(1, MAX_REPLACE_WORDS);
        if !self.current.is_empty() {
            let take_prev = replace_words.saturating_sub(1).min(self.prev_words.len());
            let first_prev = self.prev_words.len() - take_prev;
            for word in self.prev_words.iter_mut().skip(first_prev) {
                mark_word_layout(word, layout_is_ru);
            }
            mark_word_layout(&mut self.current, layout_is_ru);
        } else if self.prev_had_trailing_space && !self.prev_words.is_empty() {
            let take_prev = replace_words.min(self.prev_words.len());
            let first_prev = self.prev_words.len() - take_prev;
            for word in self.prev_words.iter_mut().skip(first_prev) {
                mark_word_layout(word, layout_is_ru);
            }
        }
        self.replay_toggle_ready = true;
    }

    fn replay_toggle_ready(&self) -> bool {
        self.replay_toggle_ready
    }

    fn remember_inserted_tail_for_replay(
        &mut self,
        original_events: &[KeyEvent],
        plan: &TextReplacement,
        inserted_layout_is_ru: bool,
    ) -> bool {
        if plan.move_right != 0 || plan.insert.is_empty() {
            return false;
        }

        let replaced_len = plan.backspaces as usize;
        if replaced_len == 0 || replaced_len > original_events.len() {
            return false;
        }

        let start = original_events.len() - replaced_len;
        let mut tail = original_events[start..].to_vec();
        if tail.is_empty()
            || tail
                .iter()
                .any(|ev| ev.keycode == KeyCode::KEY_SPACE.code())
        {
            return false;
        }

        mark_word_layout(&mut tail, inserted_layout_is_ru);
        if map_original_events(&tail) != plan.insert {
            return false;
        }

        self.current = tail;
        self.prev_words.clear();
        self.prev_had_trailing_space = false;
        self.replay_toggle_ready = true;
        self.last_input = Instant::now();
        true
    }

    fn remember_inserted_last_word_for_replay(
        &mut self,
        original_events: &[KeyEvent],
        plan: &TextReplacement,
    ) -> bool {
        if plan.move_right != 0 || plan.insert.trim().is_empty() {
            return false;
        }

        let Some(inserted_word) = plan.insert.split_whitespace().next_back() else {
            return false;
        };
        if inserted_word.is_empty() {
            return false;
        }

        let Some(words) = split_event_words(original_events) else {
            return false;
        };
        for word in words.iter().rev() {
            for target_is_ru in [false, true] {
                if map_events_to_layout(word, target_is_ru) != inserted_word {
                    continue;
                }

                let mut tail = (*word).to_vec();
                mark_word_layout(&mut tail, target_is_ru);
                self.current = tail;
                self.prev_words.clear();
                self.prev_had_trailing_space = false;
                self.replay_toggle_ready = true;
                self.last_input = Instant::now();
                return true;
            }
        }

        false
    }

    fn remember_replacement_last_word_for_replay(
        &mut self,
        original_events: &[KeyEvent],
        plan: &TextReplacement,
        replacement: &str,
    ) -> bool {
        if plan.move_right != 0 || plan.backspaces == 0 {
            return false;
        }

        let Some(inserted_word) = replacement.split_whitespace().next_back() else {
            return false;
        };
        if inserted_word.is_empty() {
            return false;
        }

        let Some(words) = split_event_words(original_events) else {
            return false;
        };
        for word in words.iter().rev() {
            for target_is_ru in [false, true] {
                if map_events_to_layout(word, target_is_ru) != inserted_word {
                    continue;
                }

                let mut tail = (*word).to_vec();
                mark_word_layout(&mut tail, target_is_ru);
                self.current = tail;
                self.prev_words.clear();
                self.prev_had_trailing_space = false;
                self.replay_toggle_ready = true;
                self.last_input = Instant::now();
                return true;
            }
        }

        false
    }

    fn remember_pending_learning_correction(
        &mut self,
        lay_kind: &str,
        lay_from: &str,
        lay_to: &str,
        replace_words: usize,
        words: usize,
    ) {
        if lay_from == lay_to || lay_from.trim().is_empty() || lay_to.trim().is_empty() {
            self.pending_learning = None;
            return;
        }

        self.pending_learning = Some(PendingLearningCorrection {
            lay_kind: lay_kind.to_string(),
            lay_from: lay_from.to_string(),
            lay_to: lay_to.to_string(),
            replace_words,
            words,
            started_at: Instant::now(),
            deleted_chars: 0,
            typed: Vec::new(),
        });
    }

    fn note_learning_backspace(&mut self) {
        let Some(pending) = self.pending_learning.as_mut() else {
            return;
        };
        if pending.started_at.elapsed() > Duration::from_secs(LEARNING_FEEDBACK_MAX_AGE_SECS) {
            self.pending_learning = None;
            return;
        }
        pending.deleted_chars = pending.deleted_chars.saturating_add(1);
        pending.typed.clear();
    }

    fn note_learning_typed(&mut self, event: KeyEvent) {
        let Some(pending) = self.pending_learning.as_mut() else {
            return;
        };
        if pending.started_at.elapsed() > Duration::from_secs(LEARNING_FEEDBACK_MAX_AGE_SECS) {
            self.pending_learning = None;
            return;
        }
        if pending.deleted_chars == 0 {
            self.pending_learning = None;
            return;
        }
        pending.typed.push(event);
    }

    fn take_user_learning_correction(
        &mut self,
        include_trailing_space: bool,
    ) -> Option<UserLearningCorrection> {
        let pending = self.pending_learning.take()?;
        if pending.deleted_chars == 0 || pending.typed.is_empty() {
            return None;
        }

        let from = tail_chars(&pending.lay_to, pending.deleted_chars as usize);
        let mut to = map_original_events(&pending.typed);
        let lay_to_ends_with_space = pending
            .lay_to
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        if include_trailing_space && lay_to_ends_with_space {
            to.push(' ');
        }

        if from == to || from.trim().is_empty() || to.trim().is_empty() {
            return None;
        }

        Some(UserLearningCorrection {
            lay_kind: pending.lay_kind,
            lay_from: pending.lay_from,
            lay_to: pending.lay_to,
            from,
            to,
            replace_words: pending.replace_words,
            words: pending.words,
        })
    }

    /// Что ре-печатать при двойном Shift и сколько backspaces.
    fn what_to_replay(&self, replace_words: usize) -> Option<(Vec<KeyEvent>, u32)> {
        let replace_words = replace_words.clamp(1, MAX_REPLACE_WORDS);
        if !self.current.is_empty() {
            let take_prev = replace_words.saturating_sub(1).min(self.prev_words.len());
            let mut events = Vec::new();
            for word in self
                .prev_words
                .iter()
                .skip(self.prev_words.len() - take_prev)
            {
                if !events.is_empty() {
                    events.push(KeyEvent {
                        keycode: KeyCode::KEY_SPACE.code(),
                        shift: false,
                        layout_is_ru: false,
                    });
                }
                events.extend(word.iter().copied());
            }
            if !events.is_empty() {
                events.push(KeyEvent {
                    keycode: KeyCode::KEY_SPACE.code(),
                    shift: false,
                    layout_is_ru: false,
                });
            }
            events.extend(self.current.iter().copied());
            let n = events.len() as u32;
            Some((events, n))
        } else if self.prev_had_trailing_space && !self.prev_words.is_empty() {
            let take_prev = replace_words.min(self.prev_words.len());
            let mut events = Vec::new();
            for word in self
                .prev_words
                .iter()
                .skip(self.prev_words.len() - take_prev)
            {
                if !events.is_empty() {
                    events.push(KeyEvent {
                        keycode: KeyCode::KEY_SPACE.code(),
                        shift: false,
                        layout_is_ru: false,
                    });
                }
                events.extend(word.iter().copied());
            }
            events.push(KeyEvent {
                keycode: KeyCode::KEY_SPACE.code(),
                shift: false,
                layout_is_ru: false,
            });
            let n = events.len() as u32;
            Some((events, n))
        } else {
            None
        }
    }
}

fn mark_word_layout(word: &mut [KeyEvent], layout_is_ru: bool) {
    for event in word {
        if is_typing_key(KeyCode::new(event.keycode)) {
            event.layout_is_ru = layout_is_ru;
        }
    }
}

// ─── Word boundary детекция ─────────────────────────────────

fn is_hard_boundary(key: KeyCode) -> bool {
    use KeyCode as K;
    matches!(
        key,
        K::KEY_ENTER
            | K::KEY_TAB
            | K::KEY_ESC
            | K::KEY_LEFT
            | K::KEY_RIGHT
            | K::KEY_UP
            | K::KEY_DOWN
            | K::KEY_HOME
            | K::KEY_END
            | K::KEY_PAGEUP
            | K::KEY_PAGEDOWN
            | K::KEY_BACKSPACE
            | K::KEY_DELETE
    )
}

/// Клавиша которая порождает символ в текстовом поле (с учётом раскладки).
fn is_typing_key(key: KeyCode) -> bool {
    use KeyCode as K;
    matches!(
        key,
        K::KEY_A
            | K::KEY_B
            | K::KEY_C
            | K::KEY_D
            | K::KEY_E
            | K::KEY_F
            | K::KEY_G
            | K::KEY_H
            | K::KEY_I
            | K::KEY_J
            | K::KEY_K
            | K::KEY_L
            | K::KEY_M
            | K::KEY_N
            | K::KEY_O
            | K::KEY_P
            | K::KEY_Q
            | K::KEY_R
            | K::KEY_S
            | K::KEY_T
            | K::KEY_U
            | K::KEY_V
            | K::KEY_W
            | K::KEY_X
            | K::KEY_Y
            | K::KEY_Z
            | K::KEY_1
            | K::KEY_2
            | K::KEY_3
            | K::KEY_4
            | K::KEY_5
            | K::KEY_6
            | K::KEY_7
            | K::KEY_8
            | K::KEY_9
            | K::KEY_0
            | K::KEY_SEMICOLON
            | K::KEY_APOSTROPHE
            | K::KEY_COMMA
            | K::KEY_DOT
            | K::KEY_LEFTBRACE
            | K::KEY_RIGHTBRACE
            | K::KEY_GRAVE
            | K::KEY_SLASH
            | K::KEY_BACKSLASH
            | K::KEY_MINUS
            | K::KEY_EQUAL
    )
}

fn should_ignore_buffer_key(key: KeyCode, modifiers: &ShiftState, current_empty: bool) -> bool {
    if modifiers.shortcut_active() && (key == KeyCode::KEY_SPACE || is_typing_key(key)) {
        return true;
    }

    current_empty && is_leading_non_word_symbol_key(key, modifiers.any())
}

fn is_leading_non_word_symbol_key(key: KeyCode, _shift: bool) -> bool {
    matches!(key, KeyCode::KEY_EQUAL | KeyCode::KEY_MINUS)
}

// ─── Двойной Shift handler ──────────────────────────────────

fn handle_typing_assist_after_space(
    buf: &mut WordBuffer,
    virtual_kbd: Option<&mut VirtualDevice>,
    executing: &mut bool,
) {
    let started_at = Instant::now();
    let correction = [2, 1].into_iter().find_map(|word_count| {
        let events = buf.last_completed_words_events(word_count)?;
        let original = map_original_events(&events);
        let replacement = apply_typing_assist(&original, active_auto_switch_layout())?;
        Some((original, replacement))
    });
    let Some((original, replacement)) = correction else {
        return;
    };

    let Some(kbd) = virtual_kbd else {
        log("⚠ typing-assist: нет uinput device");
        return;
    };

    *executing = true;
    let _executing_guard = ExecutingGuard(executing);

    if let Err(e) = release_possible_modifiers(kbd) {
        log(&format!("⚠ typing-assist modifier cleanup failed: {e}"));
    }

    let original_layout = read_current_layout_is_ru().ok();
    let plan = plan_text_replacement(&original, &replacement).unwrap_or_else(|| TextReplacement {
        move_left: 0,
        backspaces: original.chars().count() as u32,
        insert: replacement.clone(),
        move_right: 0,
    });

    if let Err(e) = apply_text_replacement(kbd, &plan) {
        log(&format!("⚠ typing-assist minimal replace failed: {e}"));
        return;
    }

    let replacement_layout_is_ru = preferred_layout_for_text(&plan.insert, true);
    if let Err(e) = insert_text_via_uinput_or_type_text(kbd, &plan.insert, replacement_layout_is_ru)
    {
        log(&format!("⚠ typing-assist text insert failed: {e}"));
        if let Err(e) = emit_key_taps_fast(kbd, KeyCode::KEY_RIGHT, plan.move_right) {
            log(&format!(
                "⚠ typing-assist cursor restore failed after insert error: {e}"
            ));
        }
        return;
    }
    if let Err(e) = emit_key_taps_fast(kbd, KeyCode::KEY_RIGHT, plan.move_right) {
        log(&format!("⚠ typing-assist cursor restore failed: {e}"));
    }
    let target_layout = preferred_layout_for_text(&replacement, replacement_layout_is_ru);
    if active_auto_switch_layout() {
        match switch_to_target_layout(target_layout) {
            Ok(layout_id) => log(&format!("  typing-assist layout → {layout_id}")),
            Err(e) => log(&format!("⚠ typing-assist layout switch failed: {e}")),
        }
    } else if let Some(layout_is_ru) = original_layout {
        match switch_to_target_layout(layout_is_ru) {
            Ok(layout_id) => log(&format!("  typing-assist layout restored → {layout_id}")),
            Err(e) => log(&format!("⚠ typing-assist layout restore failed: {e}")),
        }
    }

    let words = original.split_whitespace().count();
    buf.remember_pending_learning_correction(
        "typing-assist",
        &original,
        &replacement,
        words,
        words,
    );
    buf.reset_all();
    log(&format!(
        "✓ done: помощь при наборе {:?} → {:?} за {}ms",
        original,
        replacement,
        started_at.elapsed().as_millis()
    ));
}

fn handle_double_shift(
    buf: &mut WordBuffer,
    replace_words: usize,
    engine: CorrectionEngine,
    auto_replace: bool,
    virtual_kbd: Option<&mut VirtualDevice>,
    executing: &mut bool,
) -> Option<bool> {
    let started_at = Instant::now();
    let replace_words = effective_replace_words(buf, replace_words, engine, auto_replace);
    let Some((events, n_backspaces)) = buf.what_to_replay(replace_words) else {
        log("👆 двойной Shift, но буфер пуст");
        return None;
    };
    *executing = true; // блокируем Shift events на время выполнения
    let _executing_guard = ExecutingGuard(executing);

    let layout_decision = replay_layout_decision(&events);
    let target_is_ru = layout_decision.target_is_ru;
    let mixed_layouts = layout_decision.mixed_layouts;

    // 3-й счёт: попытаться смаппить каждый keycode → char в ОБЕ раскладки.
    // Если char_count != events.len() — какой-то keycode вне таблиц
    // keycode_to_*_char (значит backspace×N сотрёт лишнее ИЛИ замаппится не всё).
    let mapped_orig: String = events
        .iter()
        .filter_map(|ev| {
            if ev.layout_is_ru {
                keycode_to_ru_char(ev.keycode, ev.shift)
            } else {
                keycode_to_us_char(ev.keycode, ev.shift)
            }
        })
        .collect();
    let mapped_target: String = events
        .iter()
        .filter_map(|ev| {
            if target_is_ru {
                keycode_to_ru_char(ev.keycode, ev.shift)
            } else {
                keycode_to_us_char(ev.keycode, ev.shift)
            }
        })
        .collect();
    let chars_orig = mapped_orig.chars().count();
    let chars_target = mapped_target.chars().count();
    let words_orig = mapped_orig.split_whitespace().count();
    let mismatch = chars_orig != events.len() || chars_target != events.len();
    log(&format!(
        "👆 events={} n_bs={n_backspaces} | chars_orig={chars_orig} chars_target={chars_target} words={words_orig} {} mixed={} | orig={mapped_orig:?} → target={mapped_target:?}",
        events.len(),
        if mismatch { "⚠ MAP-MISMATCH" } else { "✓" },
        mixed_layouts,
    ));

    if mapped_target.is_empty() {
        log("⚠ mapped_target пуст — не вставляем");
        return None;
    }
    // ═══ АЛГОРИТМ: decision layer → backspace → replay/text insert ═══

    let force_short_replay = should_force_replay_for_short_fragment(&mapped_orig);
    let force_replay_toggle =
        engine == CorrectionEngine::Smart && (buf.replay_toggle_ready() || force_short_replay);
    let mut correction = if force_replay_toggle {
        log("  smart: replay без модели");
        Correction::ReplayAll
    } else if engine == CorrectionEngine::Smart {
        decide_scoped_tail_correction(&events)
            .map(Correction::InsertText)
            .unwrap_or_else(|| decide_correction(&mapped_orig, &mapped_target, engine))
    } else {
        decide_correction(&mapped_orig, &mapped_target, engine)
    };
    let mut insert_kind = if matches!(&correction, Correction::InsertText(_)) {
        "smart-text"
    } else {
        "auto-replace"
    };

    if correction == Correction::ReplayAll && auto_replace {
        if let Some(text) = apply_auto_replace(&mapped_orig, &mapped_target) {
            if text != mapped_target && text != mapped_orig {
                correction = Correction::InsertText(text);
                insert_kind = "auto-replace";
            }
        }
    }

    let kbd = match virtual_kbd {
        Some(k) => k,
        None => {
            log("⚠ нет uinput device");
            return None;
        }
    };
    if let Err(e) = release_possible_modifiers(kbd) {
        log(&format!("⚠ modifier cleanup before backspace failed: {e}"));
    }

    if let Correction::InsertText(text) = correction {
        let kind = insert_kind;
        if text.trim().is_empty() || text == mapped_target {
            log("  2. text decision совпал с replay — replay для сохранения toggle");
        } else {
            let plan =
                plan_text_replacement(&mapped_orig, &text).unwrap_or_else(|| TextReplacement {
                    move_left: 0,
                    backspaces: n_backspaces,
                    insert: text.clone(),
                    move_right: 0,
                });
            if let Err(e) = apply_text_replacement(kbd, &plan) {
                log(&format!("⚠ {kind} minimal replace failed: {e}"));
                return None;
            } else if let Err(e) = insert_text_via_uinput_or_type_text(
                kbd,
                &plan.insert,
                preferred_layout_for_text(&plan.insert, target_is_ru),
            ) {
                log(&format!(
                    "⚠ {kind} text insert failed after minimal delete: {e}"
                ));
                if let Err(e) = emit_key_taps_fast(kbd, KeyCode::KEY_RIGHT, plan.move_right) {
                    log(&format!(
                        "⚠ {kind} cursor restore failed after TypeText error: {e}"
                    ));
                }
                return None;
            } else {
                if let Err(e) = emit_key_taps_fast(kbd, KeyCode::KEY_RIGHT, plan.move_right) {
                    log(&format!("⚠ {kind} cursor restore failed: {e}"));
                }
                let insert_target_is_ru = preferred_layout_for_text(&text, target_is_ru);
                let layout_result = switch_to_target_layout(insert_target_is_ru);
                buf.remember_pending_learning_correction(
                    kind,
                    &mapped_orig,
                    &text,
                    replace_words,
                    words_orig,
                );
                if !buf.remember_replacement_last_word_for_replay(&events, &plan, &text)
                    && !buf.remember_inserted_tail_for_replay(
                        &events,
                        &plan,
                        preferred_layout_for_text(&plan.insert, insert_target_is_ru),
                    )
                    && !buf.remember_inserted_last_word_for_replay(&events, &plan)
                {
                    buf.reset_all();
                }
                log(&format!(
                    "  1. minimal replace: left={} bs={} insert={:?} right={}",
                    plan.move_left, plan.backspaces, plan.insert, plan.move_right
                ));
                return match layout_result {
                    Ok(layout_id) => {
                        log(&format!("  2. layout → {layout_id}"));
                        log(&format!(
                            "✓ done: {kind}, исправлен BAD-диапазон за {}ms",
                            started_at.elapsed().as_millis()
                        ));
                        Some(insert_target_is_ru)
                    }
                    Err(e) => {
                        log(&format!(
                            "⚠ {kind} layout switch after text insert failed: {e}"
                        ));
                        log(&format!(
                            "✓ done: {kind}, текст исправлен, layout не подтверждён за {}ms",
                            started_at.elapsed().as_millis()
                        ));
                        None
                    }
                };
            }
        }
    }

    // ЭТАП 1: backspace через uinput (надёжно)
    if let Err(e) = emit_backspaces(kbd, n_backspaces) {
        log(&format!("⚠ Этап 1 backspaces failed: {e}"));
        return None;
    }
    log(&format!("  1. uinput Backspace × {n_backspaces}"));

    // ЭТАП 2: переключить раскладку через extension (синхронно через DBus).
    // ActivateLayout — прямой inputSources[i].activate() в JS, мгновенно.
    if let Err(e) = switch_to_target_layout(target_is_ru) {
        log(&format!("⚠ Этап 2 layout switch failed: {e}"));
        if let Err(type_error) = call_type_text(&mapped_target) {
            log(&format!(
                "⚠ fallback TypeText failed after layout switch failure: {type_error}"
            ));
        } else {
            append_learning_log(
                "layout-text-fallback",
                &mapped_orig,
                &mapped_target,
                replace_words,
                words_orig,
            );
            buf.reset_all();
            log(&format!(
                "✓ done: layout fallback text insert за {}ms",
                started_at.elapsed().as_millis()
            ));
        }
        return None;
    }
    let (layout_id, ibus_engine) = target_layout(target_is_ru);
    log(&format!("  2. layout → {layout_id}"));

    // ЭТАП 3: replay тех же keycodes — в новой раскладке дают другие символы.
    if let Err(e) = replay_keycodes(kbd, &events) {
        log(&format!("⚠ Этап 3 replay failed: {e}"));
        return Some(target_is_ru);
    }
    buf.mark_replayed_layout(replace_words, target_is_ru);
    if !force_replay_toggle && mapped_orig != mapped_target {
        append_learning_log(
            "layout-replay",
            &mapped_orig,
            &mapped_target,
            replace_words,
            words_orig,
        );
    }
    log(&format!("  3. uinput replay × {}", events.len()));

    log(&format!(
        "✓ done: раскладка {ibus_engine}, перенабрано {} клавиш за {}ms",
        events.len(),
        started_at.elapsed().as_millis()
    ));
    Some(target_is_ru)
}

fn replay_keycodes(dev: &mut VirtualDevice, events: &[KeyEvent]) -> std::io::Result<()> {
    replay_keycodes_with_pace(dev, events, KEY_PACE_MS, 0)
}

fn replay_text_insert_keycodes(
    dev: &mut VirtualDevice,
    events: &[KeyEvent],
) -> std::io::Result<()> {
    replay_keycodes_with_pace(
        dev,
        events,
        TEXT_INSERT_KEY_PACE_MS,
        TEXT_INSERT_SPACE_SETTLE_MS,
    )
}

fn replay_keycodes_with_pace(
    dev: &mut VirtualDevice,
    events: &[KeyEvent],
    key_pace_ms: u64,
    space_settle_ms: u64,
) -> std::io::Result<()> {
    let shift_l = KeyCode::KEY_LEFTSHIFT.code();

    // CRITICAL: при быстром двойном Shift physical Shift_L юзера может ещё
    // быть «зажат» в kernel/mutter modifier state (FSM сработал по release
    // event, но modifier применяется async). Если не сбросить — все
    // последующие keys получат CAPS от висящего modifier, дают «GHBDTn».
    // Принудительно emit Shift_L/Shift_R release ДО replay — overrides
    // любой stuck physical state.
    release_possible_modifiers(dev)?;

    for ev in events {
        if ev.shift {
            dev.emit(&[
                InputEvent::new(EventType::KEY.0, shift_l, 1),
                InputEvent::new(EventType::KEY.0, ev.keycode, 1),
                InputEvent::new(EventType::KEY.0, ev.keycode, 0),
                InputEvent::new(EventType::KEY.0, shift_l, 0),
            ])?;
        } else {
            dev.emit(&[
                InputEvent::new(EventType::KEY.0, ev.keycode, 1),
                InputEvent::new(EventType::KEY.0, ev.keycode, 0),
            ])?;
        }
        let settle_ms = if ev.keycode == KeyCode::KEY_SPACE.code() && space_settle_ms > 0 {
            space_settle_ms
        } else {
            key_pace_ms
        };
        std::thread::sleep(Duration::from_millis(settle_ms));
    }
    Ok(())
}

fn insert_text_via_uinput_or_type_text(
    dev: &mut VirtualDevice,
    text: &str,
    fallback_is_ru: bool,
) -> Result<TextInsertMethod, String> {
    if let Some(runs) = text_to_uinput_runs(text, fallback_is_ru) {
        for run in runs {
            switch_to_target_layout(run.target_is_ru)?;
            replay_text_insert_keycodes(dev, &run.events).map_err(|e| e.to_string())?;
        }
        return Ok(TextInsertMethod::UinputReplay);
    }

    call_type_text(text).map(|_| TextInsertMethod::TypeTextFallback)
}

fn apply_text_replacement(dev: &mut VirtualDevice, plan: &TextReplacement) -> std::io::Result<()> {
    emit_key_taps(
        dev,
        KeyCode::KEY_LEFT,
        plan.move_left,
        TEXT_REPLACE_KEY_PACE_MS,
    )?;
    emit_backspaces_for_text_replace(dev, plan.backspaces)?;
    Ok(())
}

fn emit_key_taps_fast(dev: &mut VirtualDevice, key: KeyCode, n: u32) -> std::io::Result<()> {
    emit_key_taps(dev, key, n, 0)
}

fn emit_key_taps(
    dev: &mut VirtualDevice,
    key: KeyCode,
    n: u32,
    pace_ms: u64,
) -> std::io::Result<()> {
    let code = key.code();
    for _ in 0..n {
        dev.emit(&[
            InputEvent::new(EventType::KEY.0, code, 1),
            InputEvent::new(EventType::KEY.0, code, 0),
        ])?;
        if pace_ms > 0 {
            std::thread::sleep(Duration::from_millis(pace_ms));
        }
    }
    Ok(())
}

fn release_possible_modifiers(dev: &mut VirtualDevice) -> std::io::Result<()> {
    let modifiers = [
        KeyCode::KEY_LEFTSHIFT.code(),
        KeyCode::KEY_RIGHTSHIFT.code(),
        KeyCode::KEY_LEFTCTRL.code(),
        KeyCode::KEY_RIGHTCTRL.code(),
        KeyCode::KEY_LEFTALT.code(),
        KeyCode::KEY_RIGHTALT.code(),
    ];
    let events: Vec<_> = modifiers
        .iter()
        .map(|code| InputEvent::new(EventType::KEY.0, *code, 0))
        .collect();

    for _ in 0..MODIFIER_RELEASE_ROUNDS {
        dev.emit(&events)?;
        std::thread::sleep(Duration::from_millis(MODIFIER_RELEASE_PACE_MS));
    }
    Ok(())
}

fn emit_backspaces(dev: &mut VirtualDevice, n: u32) -> std::io::Result<()> {
    let bs = KeyCode::KEY_BACKSPACE.code();

    // Длинный batch может частично теряться в Mutter/GTK при сотнях клавиш.
    // Пейсинг делает удаление детерминированным для длинных слов.
    for _ in 0..n {
        dev.emit(&[InputEvent::new(EventType::KEY.0, bs, 1)])?;
        std::thread::sleep(Duration::from_millis(BACKSPACE_DOWN_MS));
        dev.emit(&[InputEvent::new(EventType::KEY.0, bs, 0)])?;
        std::thread::sleep(Duration::from_millis(BACKSPACE_PACE_MS));
    }
    std::thread::sleep(Duration::from_millis(BACKSPACE_SETTLE_MS));
    Ok(())
}

fn emit_backspaces_for_text_replace(dev: &mut VirtualDevice, n: u32) -> std::io::Result<()> {
    let bs = KeyCode::KEY_BACKSPACE.code();
    for _ in 0..n {
        dev.emit(&[InputEvent::new(EventType::KEY.0, bs, 1)])?;
        std::thread::sleep(Duration::from_millis(TEXT_REPLACE_BACKSPACE_DOWN_MS));
        dev.emit(&[InputEvent::new(EventType::KEY.0, bs, 0)])?;
        std::thread::sleep(Duration::from_millis(TEXT_REPLACE_BACKSPACE_PACE_MS));
    }
    std::thread::sleep(Duration::from_millis(TEXT_REPLACE_BACKSPACE_SETTLE_MS));
    Ok(())
}

fn switch_ibus_engine(engine: &str) -> Result<(), String> {
    let out = Command::new("ibus")
        .args(["engine", engine])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

fn read_ibus_engine() -> Result<String, String> {
    let out = Command::new("ibus")
        .arg("engine")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_current_layout_is_ru() -> Result<bool, String> {
    call_current_layout()
        .map(|id| id == "ru")
        .or_else(|_| read_ibus_engine().map(|engine| engine.starts_with("xkb:ru")))
}

fn call_list_layouts() -> Result<String, String> {
    call_dbus_list_layouts().or_else(|fast_error| {
        reset_dbus_connection();
        log(&format!(
            "⚠ DBus fast ListLayouts failed: {fast_error}; fallback gdbus"
        ));
        run_gdbus(&format!("{DBUS_INTERFACE}.ListLayouts"), &[])
    })
}

fn parse_gdbus_string(reply: &str) -> Option<String> {
    let trimmed = reply.trim();
    let without_tuple = trimmed.strip_prefix("('")?.strip_suffix("',)")?;
    Some(without_tuple.replace("\\'", "'"))
}

fn parse_gdbus_bool(reply: &str) -> Option<bool> {
    let trimmed = reply.trim();
    match trimmed {
        "(true,)" => Some(true),
        "(false,)" => Some(false),
        _ => None,
    }
}

fn parse_current_layout_from_list(layouts: &str) -> Option<String> {
    let list = parse_gdbus_string(layouts).unwrap_or_else(|| layouts.to_string());
    list.split(',').find_map(|entry| {
        let current = entry.strip_suffix('*')?;
        current.rsplit(':').next().map(str::to_string)
    })
}

/// Преобразует keycode + shift в RU-символ (йцукен раскладка).
/// Используется когда current_layout = RU (1) — буфер keycodes
/// представляет реально набранную кириллицу.
fn keycode_to_ru_char(keycode: u16, shift: bool) -> Option<char> {
    use KeyCode as K;
    let key = KeyCode::new(keycode);
    let lower = match key {
        K::KEY_Q => Some('й'),
        K::KEY_W => Some('ц'),
        K::KEY_E => Some('у'),
        K::KEY_R => Some('к'),
        K::KEY_T => Some('е'),
        K::KEY_Y => Some('н'),
        K::KEY_U => Some('г'),
        K::KEY_I => Some('ш'),
        K::KEY_O => Some('щ'),
        K::KEY_P => Some('з'),
        K::KEY_LEFTBRACE => Some('х'),
        K::KEY_RIGHTBRACE => Some('ъ'),
        K::KEY_A => Some('ф'),
        K::KEY_S => Some('ы'),
        K::KEY_D => Some('в'),
        K::KEY_F => Some('а'),
        K::KEY_G => Some('п'),
        K::KEY_H => Some('р'),
        K::KEY_J => Some('о'),
        K::KEY_K => Some('л'),
        K::KEY_L => Some('д'),
        K::KEY_SEMICOLON => Some('ж'),
        K::KEY_APOSTROPHE => Some('э'),
        K::KEY_Z => Some('я'),
        K::KEY_X => Some('ч'),
        K::KEY_C => Some('с'),
        K::KEY_V => Some('м'),
        K::KEY_B => Some('и'),
        K::KEY_N => Some('т'),
        K::KEY_M => Some('ь'),
        K::KEY_COMMA => Some('б'),
        K::KEY_DOT => Some('ю'),
        K::KEY_GRAVE => Some('ё'),
        K::KEY_SLASH => Some('.'),
        K::KEY_1 => Some('1'),
        K::KEY_2 => Some('2'),
        K::KEY_3 => Some('3'),
        K::KEY_4 => Some('4'),
        K::KEY_5 => Some('5'),
        K::KEY_6 => Some('6'),
        K::KEY_7 => Some('7'),
        K::KEY_8 => Some('8'),
        K::KEY_9 => Some('9'),
        K::KEY_0 => Some('0'),
        K::KEY_MINUS => Some('-'),
        K::KEY_SPACE => Some(' '),
        _ => None,
    }?;
    if shift && lower.is_alphabetic() {
        lower.to_uppercase().next()
    } else {
        Some(lower)
    }
}

/// Преобразует keycode + shift в US-символ (для прогона через словарь dict).
fn keycode_to_us_char(keycode: u16, shift: bool) -> Option<char> {
    use KeyCode as K;
    let key = KeyCode::new(keycode);
    let lower = match key {
        K::KEY_A => Some('a'),
        K::KEY_B => Some('b'),
        K::KEY_C => Some('c'),
        K::KEY_D => Some('d'),
        K::KEY_E => Some('e'),
        K::KEY_F => Some('f'),
        K::KEY_G => Some('g'),
        K::KEY_H => Some('h'),
        K::KEY_I => Some('i'),
        K::KEY_J => Some('j'),
        K::KEY_K => Some('k'),
        K::KEY_L => Some('l'),
        K::KEY_M => Some('m'),
        K::KEY_N => Some('n'),
        K::KEY_O => Some('o'),
        K::KEY_P => Some('p'),
        K::KEY_Q => Some('q'),
        K::KEY_R => Some('r'),
        K::KEY_S => Some('s'),
        K::KEY_T => Some('t'),
        K::KEY_U => Some('u'),
        K::KEY_V => Some('v'),
        K::KEY_W => Some('w'),
        K::KEY_X => Some('x'),
        K::KEY_Y => Some('y'),
        K::KEY_Z => Some('z'),
        K::KEY_1 => Some('1'),
        K::KEY_2 => Some('2'),
        K::KEY_3 => Some('3'),
        K::KEY_4 => Some('4'),
        K::KEY_5 => Some('5'),
        K::KEY_6 => Some('6'),
        K::KEY_7 => Some('7'),
        K::KEY_8 => Some('8'),
        K::KEY_9 => Some('9'),
        K::KEY_0 => Some('0'),
        K::KEY_SEMICOLON => Some(';'),
        K::KEY_APOSTROPHE => Some('\''),
        K::KEY_COMMA => Some(','),
        K::KEY_DOT => Some('.'),
        K::KEY_LEFTBRACE => Some('['),
        K::KEY_RIGHTBRACE => Some(']'),
        K::KEY_GRAVE => Some('`'),
        K::KEY_SLASH => Some('/'),
        K::KEY_BACKSLASH => Some('\\'),
        K::KEY_MINUS => Some('-'),
        K::KEY_EQUAL => Some('='),
        K::KEY_SPACE => Some(' '),
        _ => None,
    }?;
    if shift && lower.is_alphabetic() {
        lower.to_uppercase().next()
    } else {
        Some(lower)
    }
}

fn char_to_layout_key_event(ch: char, current_is_ru: bool) -> Option<(bool, KeyEvent)> {
    if is_cyrillic_letter(ch) {
        return char_to_ru_key_event(ch).map(|event| (true, event));
    }
    if ch.is_ascii_alphabetic() {
        return char_to_us_key_event(ch).map(|event| (false, event));
    }
    if current_is_ru {
        if let Some(event) = char_to_ru_key_event(ch) {
            return Some((true, event));
        }
    }
    if let Some(event) = char_to_us_key_event(ch) {
        return Some((false, event));
    }
    char_to_ru_key_event(ch).map(|event| (true, event))
}

fn char_to_ru_key_event(ch: char) -> Option<KeyEvent> {
    use KeyCode as K;
    let mut chars = ch.to_lowercase();
    let lower = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let shift = ch.is_uppercase();
    let (key, force_shift) = match lower {
        'й' => (K::KEY_Q, false),
        'ц' => (K::KEY_W, false),
        'у' => (K::KEY_E, false),
        'к' => (K::KEY_R, false),
        'е' => (K::KEY_T, false),
        'н' => (K::KEY_Y, false),
        'г' => (K::KEY_U, false),
        'ш' => (K::KEY_I, false),
        'щ' => (K::KEY_O, false),
        'з' => (K::KEY_P, false),
        'х' => (K::KEY_LEFTBRACE, false),
        'ъ' => (K::KEY_RIGHTBRACE, false),
        'ф' => (K::KEY_A, false),
        'ы' => (K::KEY_S, false),
        'в' => (K::KEY_D, false),
        'а' => (K::KEY_F, false),
        'п' => (K::KEY_G, false),
        'р' => (K::KEY_H, false),
        'о' => (K::KEY_J, false),
        'л' => (K::KEY_K, false),
        'д' => (K::KEY_L, false),
        'ж' => (K::KEY_SEMICOLON, false),
        'э' => (K::KEY_APOSTROPHE, false),
        'я' => (K::KEY_Z, false),
        'ч' => (K::KEY_X, false),
        'с' => (K::KEY_C, false),
        'м' => (K::KEY_V, false),
        'и' => (K::KEY_B, false),
        'т' => (K::KEY_N, false),
        'ь' => (K::KEY_M, false),
        'б' => (K::KEY_COMMA, false),
        'ю' => (K::KEY_DOT, false),
        'ё' => (K::KEY_GRAVE, false),
        '1' => (K::KEY_1, false),
        '2' => (K::KEY_2, false),
        '3' => (K::KEY_3, false),
        '4' => (K::KEY_4, false),
        '5' => (K::KEY_5, false),
        '6' => (K::KEY_6, false),
        '7' => (K::KEY_7, false),
        '8' => (K::KEY_8, false),
        '9' => (K::KEY_9, false),
        '0' => (K::KEY_0, false),
        '-' => (K::KEY_MINUS, false),
        '.' => (K::KEY_SLASH, false),
        ',' => (K::KEY_SLASH, true),
        ' ' => (K::KEY_SPACE, false),
        _ => return None,
    };

    Some(KeyEvent {
        keycode: key.code(),
        shift: shift || force_shift,
        layout_is_ru: true,
    })
}

fn char_to_us_key_event(ch: char) -> Option<KeyEvent> {
    use KeyCode as K;
    let (key, shift) = match ch {
        'a' | 'A' => (K::KEY_A, ch.is_uppercase()),
        'b' | 'B' => (K::KEY_B, ch.is_uppercase()),
        'c' | 'C' => (K::KEY_C, ch.is_uppercase()),
        'd' | 'D' => (K::KEY_D, ch.is_uppercase()),
        'e' | 'E' => (K::KEY_E, ch.is_uppercase()),
        'f' | 'F' => (K::KEY_F, ch.is_uppercase()),
        'g' | 'G' => (K::KEY_G, ch.is_uppercase()),
        'h' | 'H' => (K::KEY_H, ch.is_uppercase()),
        'i' | 'I' => (K::KEY_I, ch.is_uppercase()),
        'j' | 'J' => (K::KEY_J, ch.is_uppercase()),
        'k' | 'K' => (K::KEY_K, ch.is_uppercase()),
        'l' | 'L' => (K::KEY_L, ch.is_uppercase()),
        'm' | 'M' => (K::KEY_M, ch.is_uppercase()),
        'n' | 'N' => (K::KEY_N, ch.is_uppercase()),
        'o' | 'O' => (K::KEY_O, ch.is_uppercase()),
        'p' | 'P' => (K::KEY_P, ch.is_uppercase()),
        'q' | 'Q' => (K::KEY_Q, ch.is_uppercase()),
        'r' | 'R' => (K::KEY_R, ch.is_uppercase()),
        's' | 'S' => (K::KEY_S, ch.is_uppercase()),
        't' | 'T' => (K::KEY_T, ch.is_uppercase()),
        'u' | 'U' => (K::KEY_U, ch.is_uppercase()),
        'v' | 'V' => (K::KEY_V, ch.is_uppercase()),
        'w' | 'W' => (K::KEY_W, ch.is_uppercase()),
        'x' | 'X' => (K::KEY_X, ch.is_uppercase()),
        'y' | 'Y' => (K::KEY_Y, ch.is_uppercase()),
        'z' | 'Z' => (K::KEY_Z, ch.is_uppercase()),
        '1' => (K::KEY_1, false),
        '2' => (K::KEY_2, false),
        '3' => (K::KEY_3, false),
        '4' => (K::KEY_4, false),
        '5' => (K::KEY_5, false),
        '6' => (K::KEY_6, false),
        '7' => (K::KEY_7, false),
        '8' => (K::KEY_8, false),
        '9' => (K::KEY_9, false),
        '0' => (K::KEY_0, false),
        '!' => (K::KEY_1, true),
        '@' => (K::KEY_2, true),
        '#' => (K::KEY_3, true),
        '$' => (K::KEY_4, true),
        '%' => (K::KEY_5, true),
        '^' => (K::KEY_6, true),
        '&' => (K::KEY_7, true),
        '*' => (K::KEY_8, true),
        '(' => (K::KEY_9, true),
        ')' => (K::KEY_0, true),
        ';' => (K::KEY_SEMICOLON, false),
        ':' => (K::KEY_SEMICOLON, true),
        '\'' => (K::KEY_APOSTROPHE, false),
        '"' => (K::KEY_APOSTROPHE, true),
        ',' => (K::KEY_COMMA, false),
        '<' => (K::KEY_COMMA, true),
        '.' => (K::KEY_DOT, false),
        '>' => (K::KEY_DOT, true),
        '[' => (K::KEY_LEFTBRACE, false),
        '{' => (K::KEY_LEFTBRACE, true),
        ']' => (K::KEY_RIGHTBRACE, false),
        '}' => (K::KEY_RIGHTBRACE, true),
        '`' => (K::KEY_GRAVE, false),
        '~' => (K::KEY_GRAVE, true),
        '/' => (K::KEY_SLASH, false),
        '?' => (K::KEY_SLASH, true),
        '\\' => (K::KEY_BACKSLASH, false),
        '|' => (K::KEY_BACKSLASH, true),
        '-' => (K::KEY_MINUS, false),
        '_' => (K::KEY_MINUS, true),
        '=' => (K::KEY_EQUAL, false),
        '+' => (K::KEY_EQUAL, true),
        ' ' => (K::KEY_SPACE, false),
        _ => return None,
    };

    Some(KeyEvent {
        keycode: key.code(),
        shift,
        layout_is_ru: false,
    })
}

fn text_to_uinput_runs(text: &str, fallback_is_ru: bool) -> Option<Vec<TextInputRun>> {
    let mut runs: Vec<TextInputRun> = Vec::new();
    let mut current_is_ru = preferred_layout_for_text(text, fallback_is_ru);

    for ch in text.chars() {
        let (target_is_ru, event) = char_to_layout_key_event(ch, current_is_ru)?;
        current_is_ru = target_is_ru;
        if let Some(run) = runs
            .last_mut()
            .filter(|run| run.target_is_ru == target_is_ru)
        {
            run.events.push(event);
        } else {
            runs.push(TextInputRun {
                target_is_ru,
                events: vec![event],
            });
        }
    }

    Some(runs)
}

// ─── uinput re-typing ──────────────────────────────────────

fn make_virtual_keyboard() -> std::io::Result<VirtualDevice> {
    use KeyCode as K;
    // Перечисляем все клавиши которые виртуальное устройство сможет генерировать.
    let mut keys = AttributeSet::new();
    let typing = [
        K::KEY_A,
        K::KEY_B,
        K::KEY_C,
        K::KEY_D,
        K::KEY_E,
        K::KEY_F,
        K::KEY_G,
        K::KEY_H,
        K::KEY_I,
        K::KEY_J,
        K::KEY_K,
        K::KEY_L,
        K::KEY_M,
        K::KEY_N,
        K::KEY_O,
        K::KEY_P,
        K::KEY_Q,
        K::KEY_R,
        K::KEY_S,
        K::KEY_T,
        K::KEY_U,
        K::KEY_V,
        K::KEY_W,
        K::KEY_X,
        K::KEY_Y,
        K::KEY_Z,
        K::KEY_1,
        K::KEY_2,
        K::KEY_3,
        K::KEY_4,
        K::KEY_5,
        K::KEY_6,
        K::KEY_7,
        K::KEY_8,
        K::KEY_9,
        K::KEY_0,
        K::KEY_SPACE,
        K::KEY_SEMICOLON,
        K::KEY_APOSTROPHE,
        K::KEY_COMMA,
        K::KEY_DOT,
        K::KEY_LEFTBRACE,
        K::KEY_RIGHTBRACE,
        K::KEY_GRAVE,
        K::KEY_SLASH,
        K::KEY_BACKSLASH,
        K::KEY_MINUS,
        K::KEY_EQUAL,
        K::KEY_LEFTSHIFT,
        K::KEY_RIGHTSHIFT,
        K::KEY_LEFTALT,
        K::KEY_RIGHTALT,
        K::KEY_LEFTCTRL,
        K::KEY_RIGHTCTRL,
        K::KEY_INSERT,
        K::KEY_LEFT,
        K::KEY_RIGHT,
        K::KEY_BACKSPACE, // для удаления слова с экрана (cut этап)
    ];
    for k in typing.iter() {
        keys.insert(*k);
    }

    VirtualDevice::builder()?
        .name("lay-virtual-keyboard")
        .with_keys(&keys)?
        .build()
}

// ─── DBus и ibus ────────────────────────────────────────────

fn call_ping() -> Result<String, String> {
    call_dbus_ping().or_else(|fast_error| {
        reset_dbus_connection();
        log(&format!(
            "⚠ DBus fast Ping failed: {fast_error}; fallback gdbus"
        ));
        let reply = run_gdbus(&format!("{DBUS_INTERFACE}.Ping"), &[])?;
        parse_gdbus_string(&reply).ok_or_else(|| format!("не распарсил Ping: {reply}"))
    })
}

fn call_activate_layout(id: &str) -> Result<bool, String> {
    call_dbus_activate_layout(id).or_else(|fast_error| {
        reset_dbus_connection();
        log(&format!(
            "⚠ DBus fast ActivateLayout failed: {fast_error}; fallback gdbus"
        ));
        let reply = run_gdbus(
            &format!("{DBUS_INTERFACE}.ActivateLayout"),
            &[&format!("\"{id}\"")],
        )?;
        parse_gdbus_bool(&reply).ok_or_else(|| format!("не распарсил ActivateLayout: {reply}"))
    })
}

fn switch_to_layout(layout_id: &str, ibus_engine: &str, target_is_ru: bool) -> Result<(), String> {
    if !call_activate_layout(layout_id)? {
        return Err("ActivateLayout returned false".to_string());
    }

    let ibus_error = switch_ibus_engine(ibus_engine).err();
    if verify_current_layout(target_is_ru) {
        if let Some(error) = ibus_error {
            log(&format!(
                "⚠ SetGlobalEngine failed, GNOME layout verified: {error}"
            ));
        }
        return Ok(());
    }

    Err(match ibus_error {
        Some(error) => format!("SetGlobalEngine failed: {error}; layout verify failed"),
        None => "layout verify failed".to_string(),
    })
}

fn switch_to_target_layout(target_is_ru: bool) -> Result<&'static str, String> {
    let (layout_id, ibus_engine) = target_layout(target_is_ru);
    switch_to_layout(layout_id, ibus_engine, target_is_ru).map(|()| layout_id)
}

fn target_layout(target_is_ru: bool) -> (&'static str, &'static str) {
    if target_is_ru {
        ("ru", "xkb:ru::rus")
    } else {
        ("us", "xkb:us::eng")
    }
}

fn verify_current_layout(target_is_ru: bool) -> bool {
    for _ in 0..5 {
        if read_current_layout_is_ru().is_ok_and(|current| current == target_is_ru) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn call_type_text(text: &str) -> Result<String, String> {
    call_dbus_type_text(text).or_else(|fast_error| {
        reset_dbus_connection();
        log(&format!(
            "⚠ DBus fast TypeText failed: {fast_error}; fallback gdbus"
        ));
        call_type_text_gdbus(text)
    })
}

fn call_type_text_gdbus(text: &str) -> Result<String, String> {
    let arg = gvariant_string(text);
    run_gdbus(&format!("{DBUS_INTERFACE}.TypeText"), &[&arg])
}

fn dbus_connection() -> Result<zbus::blocking::Connection, String> {
    let cell = DBUS_CONNECTION.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().map_err(|e| e.to_string())?;
    if let Some(conn) = guard.as_ref() {
        return Ok(conn.clone());
    }

    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    *guard = Some(conn.clone());
    Ok(conn)
}

fn reset_dbus_connection() {
    if let Some(cell) = DBUS_CONNECTION.get() {
        if let Ok(mut guard) = cell.lock() {
            *guard = None;
        }
    }
}

fn call_dbus_ping() -> Result<String, String> {
    let reply = dbus_connection()?
        .call_method(
            Some(DBUS_DEST),
            DBUS_PATH,
            Some(DBUS_INTERFACE),
            "Ping",
            &(),
        )
        .map_err(|e| e.to_string())?;
    reply
        .body()
        .deserialize::<String>()
        .map_err(|e| e.to_string())
}

fn call_dbus_type_text(text: &str) -> Result<String, String> {
    dbus_connection()?
        .call_method(
            Some(DBUS_DEST),
            DBUS_PATH,
            Some(DBUS_INTERFACE),
            "TypeText",
            &text,
        )
        .map_err(|e| e.to_string())?;
    Ok(String::new())
}

fn call_dbus_activate_layout(id: &str) -> Result<bool, String> {
    let reply = dbus_connection()?
        .call_method(
            Some(DBUS_DEST),
            DBUS_PATH,
            Some(DBUS_INTERFACE),
            "ActivateLayout",
            &id,
        )
        .map_err(|e| e.to_string())?;
    reply
        .body()
        .deserialize::<bool>()
        .map_err(|e| e.to_string())
}

fn call_current_layout() -> Result<String, String> {
    call_dbus_current_layout().or_else(|fast_error| {
        reset_dbus_connection();
        log(&format!(
            "⚠ DBus fast CurrentLayout failed: {fast_error}; fallback gdbus"
        ));
        let current = run_gdbus(&format!("{DBUS_INTERFACE}.CurrentLayout"), &[]);
        match current {
            Ok(reply) => parse_gdbus_string(&reply).ok_or_else(|| format!("не распарсил: {reply}")),
            Err(current_error) => {
                let layouts = call_list_layouts()
                    .map_err(|list_error| format!("{current_error}; ListLayouts: {list_error}"))?;
                parse_current_layout_from_list(&layouts)
                    .ok_or_else(|| format!("не нашёл текущую раскладку: {layouts}"))
            }
        }
    })
}

fn call_dbus_current_layout() -> Result<String, String> {
    let reply = dbus_connection()?
        .call_method(
            Some(DBUS_DEST),
            DBUS_PATH,
            Some(DBUS_INTERFACE),
            "CurrentLayout",
            &(),
        )
        .map_err(|e| e.to_string())?;
    reply
        .body()
        .deserialize::<String>()
        .map_err(|e| e.to_string())
}

fn call_dbus_list_layouts() -> Result<String, String> {
    let reply = dbus_connection()?
        .call_method(
            Some(DBUS_DEST),
            DBUS_PATH,
            Some(DBUS_INTERFACE),
            "ListLayouts",
            &(),
        )
        .map_err(|e| e.to_string())?;
    reply
        .body()
        .deserialize::<String>()
        .map_err(|e| e.to_string())
}

fn gvariant_string(text: &str) -> String {
    format!("{text:?}")
}

fn run_gdbus(method: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd_args = vec![
        "call",
        "--session",
        "--dest",
        DBUS_DEST,
        "--object-path",
        DBUS_PATH,
        "--method",
        method,
    ];
    cmd_args.extend(args);
    let out = Command::new("gdbus")
        .args(&cmd_args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ─── Поиск устройства клавиатуры ────────────────────────────

fn find_all_keyboards() -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/dev/input")? {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|s| s.starts_with("event"))
        {
            continue;
        }
        if let Ok(dev) = Device::open(&path) {
            if let Some(keys) = dev.supported_keys() {
                if keys.contains(KeyCode::KEY_LEFTSHIFT) && keys.contains(KeyCode::KEY_A) {
                    // НЕ слушаем наше СОБСТВЕННОЕ uinput устройство (feedback loop).
                    // Имя точно "lay-virtual-keyboard" — другие "lay-*" (например тестер)
                    // должны подхватываться.
                    let name = dev.name().unwrap_or("").to_string();
                    if name == "lay-virtual-keyboard" {
                        continue;
                    }
                    found.push(path);
                }
            }
        }
    }
    if found.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "клавиатура не найдена. Возможно нет группы input — проверь `id`",
        ));
    }
    Ok(found)
}

// ─── Лог ────────────────────────────────────────────────────

static LOG_ENABLED: OnceLock<bool> = OnceLock::new();

fn set_log_enabled(enabled: bool) {
    let env_enabled = std::env::var("LAY_DEBUG_LOG")
        .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"));
    let _ = LOG_ENABLED.set(enabled || env_enabled);
}

fn log(msg: &str) {
    if !*LOG_ENABLED.get_or_init(|| {
        std::env::var("LAY_DEBUG_LOG")
            .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
    }) {
        return;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = format!("[{ts}] {msg}\n");
    eprint!("{line}");
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ─── Быстрые точные автоподмены ─────────────────────────────

fn should_expand_auto_replace_context(buf: &WordBuffer) -> bool {
    let Some((events, _)) = buf.what_to_replay(2) else {
        return false;
    };
    contains_visual_b_word(&map_original_events(&events))
}

fn map_original_events(events: &[KeyEvent]) -> String {
    events
        .iter()
        .filter_map(|ev| {
            if ev.layout_is_ru {
                keycode_to_ru_char(ev.keycode, ev.shift)
            } else {
                keycode_to_us_char(ev.keycode, ev.shift)
            }
        })
        .collect()
}

#[cfg(test)]
fn map_opposite_events(events: &[KeyEvent]) -> String {
    events
        .iter()
        .filter_map(|ev| {
            if ev.layout_is_ru {
                keycode_to_us_char(ev.keycode, ev.shift)
            } else {
                keycode_to_ru_char(ev.keycode, ev.shift)
            }
        })
        .collect()
}

fn map_events_to_layout(events: &[KeyEvent], target_is_ru: bool) -> String {
    events
        .iter()
        .filter_map(|ev| {
            if target_is_ru {
                keycode_to_ru_char(ev.keycode, ev.shift)
            } else {
                keycode_to_us_char(ev.keycode, ev.shift)
            }
        })
        .collect()
}

fn preferred_layout_for_text(text: &str, fallback_is_ru: bool) -> bool {
    text.chars()
        .rev()
        .find_map(|ch| {
            if matches!(ch, 'А'..='я' | 'ё' | 'Ё') {
                Some(true)
            } else if ch.is_ascii_alphabetic() {
                Some(false)
            } else {
                None
            }
        })
        .unwrap_or(fallback_is_ru)
}

fn tail_chars(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

fn apply_auto_replace(original: &str, target: &str) -> Option<String> {
    let (target_leading, target_core, target_trailing) = split_edge_whitespace(target);
    if target_core.is_empty() {
        return None;
    }

    if let Some(visual) = replace_visual_b_in_context(original, target) {
        return Some(visual);
    }

    replacement_for_token(target_core).map(|replacement| {
        let mut out = String::with_capacity(target.len().max(replacement.len()));
        out.push_str(target_leading);
        out.push_str(&replacement);
        out.push_str(target_trailing);
        out
    })
}

#[cfg(test)]
fn apply_typing_assist_exact(text: &str) -> Option<String> {
    apply_typing_assist(text, false)
}

fn apply_typing_assist(text: &str, allow_layout_auto: bool) -> Option<String> {
    let (leading, core, trailing) = split_edge_whitespace(text);
    if core.is_empty() {
        return None;
    }

    let replacement = correct_moved_prefix_letter_pair(core)
        .or_else(|| correct_split_word_pair(core))
        .or_else(|| replacement_for_token(core))
        .or_else(|| {
            let (token_leading, word, token_trailing) = split_word_punctuation(core);
            if word.is_empty() {
                return None;
            }
            replacement_for_token(word)
                .or_else(|| correct_duplicate_layout_prefix_on_ascii_token(word))
                .or_else(|| correct_wrong_layout_ascii_technical_token(word))
                .or_else(|| {
                    allow_layout_auto
                        .then(|| correct_wrong_layout_ascii_word(word))
                        .flatten()
                })
                .or_else(|| correct_cyrillic_word_case(word))
                .or_else(|| correct_hard_sign_typo(word))
                .or_else(|| correct_adjacent_transposition(word))
                .or_else(|| correct_repeated_letter(word))
                .or_else(|| correct_single_letter_substitution(word))
                .or_else(|| correct_missing_letter(word))
                .map(|replacement| format!("{token_leading}{replacement}{token_trailing}"))
        })?;

    let mut out = String::with_capacity(text.len().max(replacement.len()));
    out.push_str(leading);
    out.push_str(&replacement);
    out.push_str(trailing);
    (out != text).then_some(out)
}

fn correct_wrong_layout_ascii_word(token: &str) -> Option<String> {
    if !is_plain_ascii_layout_token(token) || is_protected_ascii_layout_token(token) {
        return None;
    }

    let converted = lay::dict::convert(token, lay::dict::Direction::Us2Ru);
    if converted == token || !is_cyrillic_word(&converted) {
        return None;
    }

    let converted_lower = converted.to_lowercase();
    if !is_known_russian_layout_autoswitch_word(&converted_lower) {
        return None;
    }

    match lay::llm::choose_token_hybrid(token, &converted) {
        Ok(Some(choice)) if choice == converted => Some(converted),
        Ok(Some(choice)) if choice == token => allow_short_layout_word(token, &converted_lower)
            .then(|| apply_word_case(token, &converted_lower)),
        _ => Some(apply_word_case(token, &converted_lower)),
    }
}

fn is_plain_ascii_layout_token(token: &str) -> bool {
    token.is_ascii()
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
        && !token.chars().any(|ch| ch.is_ascii_digit())
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || matches!(ch, ',' | ';' | '\'' | '[' | ']' | '`'))
}

fn is_protected_ascii_layout_token(token: &str) -> bool {
    token.chars().any(|ch| ch.is_ascii_alphabetic())
        && (is_upper_ascii_layout_acronym(token) || is_mixed_case_ascii_layout_brand(token))
}

fn is_upper_ascii_layout_acronym(token: &str) -> bool {
    let letters: Vec<char> = token
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect();
    (2..=4).contains(&letters.len()) && letters.iter().all(|ch| ch.is_ascii_uppercase())
}

fn is_mixed_case_ascii_layout_brand(token: &str) -> bool {
    let letters: Vec<char> = token
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect();
    letters.len() >= 4
        && letters.iter().any(|ch| ch.is_ascii_lowercase())
        && letters.iter().skip(1).any(|ch| ch.is_ascii_uppercase())
}

fn allow_short_layout_word(original: &str, converted_lower: &str) -> bool {
    original
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count()
        <= 3
        && russian_tiny_dictionary().contains(converted_lower)
}

fn is_known_russian_layout_autoswitch_word(word: &str) -> bool {
    let len = word.chars().filter(|ch| is_cyrillic_letter(*ch)).count();
    if len <= 3 {
        return russian_tiny_dictionary().contains(word);
    }

    is_known_russian_word_or_form(word) || russian_short_dictionary().contains(word)
}

fn replacement_for_token(token: &str) -> Option<String> {
    if let Some(replacement) = promoted_replacement_for_token(token) {
        return Some(replacement);
    }

    if let Some(replacement) = replacement_rules().get(token) {
        return Some(replacement.clone());
    }

    let lower = token.to_lowercase();
    if lower == token {
        return None;
    }
    replacement_rules()
        .get(&lower)
        .map(|replacement| apply_phrase_case(token, replacement))
}

fn promoted_replacement_for_token(token: &str) -> Option<String> {
    let rules = promoted_replacement_rules().lock().ok()?;
    if let Some(replacement) = rules.get(token) {
        return Some(replacement.clone());
    }

    let lower = token.to_lowercase();
    if lower == token {
        return None;
    }
    rules
        .get(&lower)
        .map(|replacement| apply_phrase_case(token, replacement))
}

fn promoted_replacement_rules() -> &'static Mutex<HashMap<String, String>> {
    static RULES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    RULES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_promoted_replacement(from: &str, to: &str) {
    if let Ok(mut rules) = promoted_replacement_rules().lock() {
        rules.insert(from.to_string(), to.to_string());
    }
}

fn correct_duplicate_layout_prefix_on_ascii_token(token: &str) -> Option<String> {
    let mut chars = token.chars();
    let first = chars.next()?;
    if !is_cyrillic_letter(first) {
        return None;
    }

    let rest: String = chars.collect();
    if !is_ascii_technical_token(&rest) {
        return None;
    }

    let mapped = lay::dict::convert(&first.to_string(), lay::dict::Direction::Ru2Us);
    let mut mapped_chars = mapped.chars();
    let mapped = mapped_chars.next()?;
    if mapped_chars.next().is_some() {
        return None;
    }

    let rest_first = rest.chars().next()?;
    if rest_first.is_ascii_alphabetic() && mapped.eq_ignore_ascii_case(&rest_first) {
        Some(rest)
    } else {
        None
    }
}

fn correct_wrong_layout_ascii_technical_token(token: &str) -> Option<String> {
    if !token.contains('-') {
        return None;
    }
    if !token.chars().any(is_cyrillic_letter) || token.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    if !token
        .chars()
        .all(|ch| is_cyrillic_letter(ch) || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.'))
    {
        return None;
    }

    let converted = lay::dict::convert(token, lay::dict::Direction::Ru2Us);
    if converted == token || !is_ascii_technical_token(&converted) {
        return None;
    }

    let has_clear_separator = converted.contains('-');
    let has_short_ascii_segment = converted
        .split(['-', '_', '.'])
        .any(|part| (2..=4).contains(&part.chars().count()));
    let original_known_hyphen_word =
        token.contains('-') && is_known_cyrillic_hyphenated_word(token);

    if has_clear_separator && has_short_ascii_segment && !original_known_hyphen_word {
        Some(converted)
    } else {
        None
    }
}

fn should_keep_plain_cyrillic_before_ascii_technical(original: &str, converted: &str) -> bool {
    original.chars().count() >= 4
        && original.chars().all(is_cyrillic_letter)
        && converted != original
        && is_ascii_technical_token(converted)
}

fn is_ascii_technical_token(token: &str) -> bool {
    token.is_ascii()
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
        && token.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '-' | '_' | '.' | '@' | '/' | '\\' | ':' | '+' | '#')
        })
        && token
            .chars()
            .any(|ch| matches!(ch, '-' | '_' | '.' | '@' | '/' | '\\' | ':' | '+' | '#'))
}

fn correct_split_word_pair(text: &str) -> Option<String> {
    let segments = split_ws_segments(text);
    if segments.len() != 3 || segments[0].1 || !segments[1].1 || segments[2].1 {
        return None;
    }

    let (left_leading, left, left_trailing) = split_word_punctuation(segments[0].0);
    let (right_leading, right, right_trailing) = split_word_punctuation(segments[2].0);
    if !left_leading.is_empty()
        || !left_trailing.is_empty()
        || !right_leading.is_empty()
        || left.is_empty()
        || right.is_empty()
    {
        return None;
    }

    let glued = format!("{left}{right}");
    if glued.chars().count() < 4 || !is_cyrillic_word(&glued) {
        return None;
    }

    let lower = glued.to_lowercase();
    if !is_known_russian_word_or_form(&lower)
        && !can_merge_split_without_dictionary(left, right, &lower, text)
    {
        return None;
    }
    if !ngram_allows_ru_candidate(&lower, text, NGRAM_SPLIT_REJECT_MARGIN) {
        return None;
    }

    Some(format!(
        "{}{}",
        apply_word_case(&glued, &lower),
        right_trailing
    ))
}

fn can_merge_split_without_dictionary(
    left: &str,
    right: &str,
    glued_lower: &str,
    text: &str,
) -> bool {
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let glued_len = glued_lower.chars().count();
    if russian_short_dictionary().contains(&right.to_lowercase()) {
        return false;
    }

    (2..=3).contains(&right_len)
        && left_len == 1
        && left.eq_ignore_ascii_case("я")
        && glued_len >= 4
        && lay::ngram::ru_candidate_margin(glued_lower, text) >= NGRAM_NODICT_SPLIT_REJECT_MARGIN
}

fn correct_moved_prefix_letter_pair(text: &str) -> Option<String> {
    let segments = split_ws_segments(text);
    if segments.len() != 3 || segments[0].1 || !segments[1].1 || segments[2].1 {
        return None;
    }

    let (left_leading, left, left_trailing) = split_word_punctuation(segments[0].0);
    let (right_leading, right, right_trailing) = split_word_punctuation(segments[2].0);
    if !left_leading.is_empty()
        || !left_trailing.is_empty()
        || !right_leading.is_empty()
        || left.is_empty()
        || right.chars().count() < 2
    {
        return None;
    }

    let mut right_chars = right.chars();
    let moved = right_chars.next()?;
    if is_known_russian_word_or_form(&right.to_lowercase()) {
        return None;
    }
    let right_rest: String = right_chars.collect();
    let left_candidate = format!("{left}{moved}");
    let candidate = format!("{left_candidate} {right_rest}");

    if !is_cyrillic_word(&left_candidate) || !is_cyrillic_word(&right_rest) {
        return None;
    }

    let left_candidate_lower = left_candidate.to_lowercase();
    let right_rest_lower = right_rest.to_lowercase();
    let right_lower = right.to_lowercase();
    let short_right_is_safe = is_safe_short_moved_prefix_right(&right_rest_lower)
        && !is_known_russian_word_or_form(&right_lower);

    if left_candidate.chars().count() >= 5
        && short_right_is_safe
        && is_known_russian_word_or_form(&left_candidate_lower)
        && ngram_allows_ru_candidate(&candidate.to_lowercase(), text, NGRAM_MOVED_PREFIX_MARGIN)
    {
        return Some(format!("{candidate}{right_trailing}"));
    }

    if let Some(left_last) = left.chars().last() {
        if short_right_is_safe
            && same_letter_ignore_case(left_last, moved)
            && is_known_russian_word_or_form(&left.to_lowercase())
        {
            let candidate = format!("{left} {right_rest}");
            if ngram_allows_ru_candidate(&candidate.to_lowercase(), text, NGRAM_MOVED_PREFIX_MARGIN)
            {
                return Some(format!("{candidate}{right_trailing}"));
            }
        }
    }

    if left_candidate.chars().count() < 5 || right_rest.chars().count() < 5 {
        return None;
    }
    let dict = russian_dictionary();
    if !dict.contains(&left_candidate_lower) || !dict.contains(&right_rest_lower) {
        return None;
    }
    if lay::ngram::ru_candidate_margin(&right_rest_lower, &right_lower)
        < NGRAM_MOVED_PREFIX_RIGHT_MARGIN
    {
        return None;
    }
    if !ngram_allows_ru_candidate(&candidate.to_lowercase(), text, NGRAM_MOVED_PREFIX_MARGIN) {
        return None;
    }

    Some(format!("{candidate}{right_trailing}"))
}

fn is_safe_short_moved_prefix_right(word: &str) -> bool {
    (3..=4).contains(&word.chars().count()) && russian_short_dictionary().contains(word)
}

fn correct_cyrillic_word_case(word: &str) -> Option<String> {
    if word.chars().count() < 2 || !is_cyrillic_word(word) {
        return None;
    }
    if word
        .chars()
        .all(|ch| !ch.is_alphabetic() || !ch.is_uppercase())
        || word
            .chars()
            .all(|ch| !ch.is_alphabetic() || ch.is_uppercase())
    {
        return None;
    }

    let lower = word.to_lowercase();
    if !is_known_russian_word_or_form(&lower) {
        return None;
    }

    let normalized = if word.chars().next().is_some_and(|ch| ch.is_uppercase()) {
        capitalize_first(&lower)
    } else {
        lower
    };
    (normalized != word).then_some(normalized)
}

fn correct_hard_sign_typo(word: &str) -> Option<String> {
    if word.chars().count() < 5 || !is_cyrillic_word(word) {
        return None;
    }

    let lower = word.to_lowercase();
    best_unique_ngram_candidate(
        word,
        generate_hard_sign_candidates(&lower),
        NGRAM_HARD_SIGN_MARGIN,
    )
}

fn correct_adjacent_transposition(word: &str) -> Option<String> {
    if word.chars().count() < 5 || !is_cyrillic_word(word) {
        return None;
    }

    let lower = word.to_lowercase();
    let dict = russian_dictionary();
    if is_known_russian_word_or_form(&lower) {
        return None;
    }

    let chars: Vec<char> = lower.chars().collect();
    let mut found: Option<String> = None;
    for idx in 0..chars.len().saturating_sub(1) {
        if chars[idx] == chars[idx + 1] {
            continue;
        }

        let mut candidate = chars.clone();
        candidate.swap(idx, idx + 1);
        let candidate: String = candidate.into_iter().collect();
        if !dict.contains(&candidate) {
            continue;
        }
        if !ngram_allows_ru_candidate(&candidate, &lower, NGRAM_TYPO_REJECT_MARGIN) {
            continue;
        }

        if found.is_some() {
            return None;
        }
        found = Some(candidate);
    }

    found.map(|candidate| apply_word_case(word, &candidate))
}

fn correct_repeated_letter(word: &str) -> Option<String> {
    if word.chars().count() < 5 || !is_cyrillic_word(word) {
        return None;
    }

    let lower = word.to_lowercase();
    let dict = russian_dictionary();
    if is_known_russian_word_or_form(&lower) {
        return None;
    }

    let chars: Vec<char> = lower.chars().collect();
    let mut found: Option<String> = None;
    let mut idx = 0;
    while idx < chars.len() {
        let mut end = idx + 1;
        while end < chars.len() && chars[end] == chars[idx] {
            end += 1;
        }

        if end - idx > 1 {
            for keep in 1..end - idx {
                let mut candidate = Vec::with_capacity(chars.len() - (end - idx - keep));
                candidate.extend_from_slice(&chars[..idx]);
                candidate.extend(std::iter::repeat(chars[idx]).take(keep));
                candidate.extend_from_slice(&chars[end..]);
                let candidate: String = candidate.into_iter().collect();
                if !dict.contains(&candidate) {
                    continue;
                }
                if !ngram_allows_ru_candidate(&candidate, &lower, NGRAM_TYPO_REJECT_MARGIN) {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                found = Some(candidate);
            }
        }

        idx = end;
    }

    found.map(|candidate| apply_word_case(word, &candidate))
}

fn correct_single_letter_substitution(word: &str) -> Option<String> {
    if word.chars().count() < 5 || !is_cyrillic_word(word) {
        return None;
    }

    let lower = word.to_lowercase();
    let dict = russian_dictionary();
    if is_known_russian_word_or_form(&lower) {
        return None;
    }

    let chars: Vec<char> = lower.chars().collect();
    let mut found: Option<String> = None;
    for idx in 0..chars.len() {
        for replacement in RU_ALPHABET {
            if replacement == chars[idx] {
                continue;
            }
            if !are_ru_keyboard_neighbors(chars[idx], replacement) {
                continue;
            }

            let mut candidate = chars.clone();
            candidate[idx] = replacement;
            let candidate: String = candidate.into_iter().collect();
            if !dict.contains(&candidate) {
                continue;
            }
            if !ngram_allows_ru_candidate(&candidate, &lower, NGRAM_TYPO_REJECT_MARGIN) {
                continue;
            }

            if found.is_some() {
                return None;
            }
            found = Some(candidate);
        }
    }

    found.map(|candidate| apply_word_case(word, &candidate))
}

fn correct_missing_letter(word: &str) -> Option<String> {
    if word.chars().count() < 6 || !is_cyrillic_word(word) {
        return None;
    }

    let lower = word.to_lowercase();
    if is_known_russian_word_or_form(&lower) {
        return None;
    }

    if let Some(candidate) = best_unique_dictionary_candidate(
        word,
        generate_missing_letter_candidates(&lower),
        NGRAM_DICT_MISSING_LETTER_MARGIN,
    ) {
        return Some(candidate);
    }

    best_unique_ngram_candidate(
        word,
        generate_missing_letter_candidates(&lower),
        NGRAM_MISSING_LETTER_MARGIN,
    )
}

fn are_ru_keyboard_neighbors(a: char, b: char) -> bool {
    let Some((row_a, col_a)) = ru_keyboard_position(a) else {
        return false;
    };
    let Some((row_b, col_b)) = ru_keyboard_position(b) else {
        return false;
    };

    row_a == row_b && col_a.abs_diff(col_b) <= 1
}

fn ru_keyboard_position(ch: char) -> Option<(usize, usize)> {
    const ROWS: [&str; 3] = ["йцукенгшщзхъ", "фывапролджэ", "ячсмитьбю"];
    ROWS.iter()
        .enumerate()
        .find_map(|(row, keys)| keys.chars().position(|key| key == ch).map(|col| (row, col)))
}

fn ngram_allows_ru_candidate(candidate: &str, baseline: &str, min_margin: f64) -> bool {
    lay::ngram::ru_candidate_margin(candidate, baseline) >= min_margin
}

fn should_force_replay_for_short_fragment(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(word) = words.next() else {
        return false;
    };
    words.next().is_none() && (1..=2).contains(&word.chars().count())
}

fn effective_replace_words(
    buf: &WordBuffer,
    replace_words: usize,
    engine: CorrectionEngine,
    auto_replace: bool,
) -> usize {
    let replace_words = replace_words.clamp(1, MAX_REPLACE_WORDS);
    if engine == CorrectionEngine::Smart && !buf.has_current_word() && !buf.replay_toggle_ready() {
        return 1;
    }
    if engine == CorrectionEngine::Replay && auto_replace && should_expand_auto_replace_context(buf)
    {
        return replace_words.max(2);
    }
    replace_words
}

fn decide_correction(original: &str, converted: &str, engine: CorrectionEngine) -> Correction {
    if engine == CorrectionEngine::Replay || original == converted {
        return Correction::ReplayAll;
    }
    if original.split_whitespace().count() <= 1 {
        return Correction::ReplayAll;
    }

    match lay::llm::convert_hybrid(original, converted) {
        // Manual double-Shift is an explicit user command. If smart says
        // "original is fine", still allow the user to toggle the selected text.
        Ok(Some(text)) if text == original => Correction::ReplayAll,
        Ok(Some(text)) if text == converted => Correction::ReplayAll,
        Ok(Some(text)) if !text.trim().is_empty() => Correction::InsertText(text),
        Ok(_) => Correction::ReplayAll,
        Err(e) => {
            log(&format!("⚠ smart decision failed: {e}; fallback на replay"));
            Correction::ReplayAll
        }
    }
}

fn decide_scoped_tail_correction(events: &[KeyEvent]) -> Option<String> {
    let words = split_event_words(events)?;
    if words.len() < 2 {
        return None;
    }

    let original = map_original_events(events);
    let mut out = String::with_capacity(original.len());
    for (idx, word) in words.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        if idx + 1 == words.len() {
            out.push_str(&flip_word_events(word));
        } else {
            out.push_str(&decide_completed_scope_word(word));
        }
    }

    if out != original && !out.trim().is_empty() {
        Some(out)
    } else {
        None
    }
}

fn split_event_words(events: &[KeyEvent]) -> Option<Vec<&[KeyEvent]>> {
    if events
        .last()
        .map_or(true, |event| event.keycode == KeyCode::KEY_SPACE.code())
    {
        return None;
    }

    let mut words = Vec::new();
    let mut start = 0;
    for (idx, event) in events.iter().enumerate() {
        if event.keycode == KeyCode::KEY_SPACE.code() {
            if start < idx {
                words.push(&events[start..idx]);
            }
            start = idx + 1;
        }
    }
    if start < events.len() {
        words.push(&events[start..]);
    }

    (!words.is_empty()).then_some(words)
}

fn decide_completed_scope_word(word: &[KeyEvent]) -> String {
    let original = map_original_events(word);
    if let Some(repaired) = correct_duplicate_layout_prefix_on_ascii_token(&original) {
        return repaired;
    }
    if let Some(repaired) = correct_wrong_layout_ascii_technical_token(&original) {
        return repaired;
    }
    let converted = flip_word_events(word);
    if should_keep_plain_cyrillic_before_ascii_technical(&original, &converted) {
        return original;
    }
    let decision = if lay::llm::model_backend_enabled() {
        lay::llm::choose_token_consensus(&original, &converted)
    } else {
        lay::llm::choose_token_hybrid(&original, &converted)
    };
    match decision {
        Ok(Some(text)) if !text.trim().is_empty() => text,
        Ok(_) | Err(_) => original,
    }
}

fn flip_word_events(word: &[KeyEvent]) -> String {
    if let Some(repaired) = repair_cyrillic_prefix_before_ascii_tail(word) {
        return repaired;
    }
    let original = map_original_events(word);
    if let Some(repaired) = correct_duplicate_layout_prefix_on_ascii_token(&original) {
        return repaired;
    }
    if let Some(target_is_ru) = mixed_visual_latin_word_target_layout(word) {
        return map_events_to_layout(word, target_is_ru);
    }
    if let Some(normalized) = normalize_mixed_word_to_last_layout(word) {
        return normalized;
    }
    let decision = replay_layout_decision(word);
    map_events_to_layout(word, decision.target_is_ru)
}

fn repair_cyrillic_prefix_before_ascii_tail(word: &[KeyEvent]) -> Option<String> {
    let first_event = word.first()?;
    let first = original_event_char(first_event)?;
    if !is_cyrillic_letter(first) || word.len() < 3 {
        return None;
    }

    let rest = &word[1..];
    let rest_original: String = rest.iter().filter_map(original_event_char).collect();
    if rest_original.chars().count() != rest.len()
        || !rest_original.is_ascii()
        || !rest_original.chars().any(|ch| ch.is_ascii_alphabetic())
        || !rest_original
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return None;
    }

    let all_ru = map_events_to_layout(word, true);
    if all_ru != map_original_events(word) && is_known_cyrillic_hyphenated_word(&all_ru) {
        return Some(all_ru);
    }

    let mut chars = all_ru.chars();
    let first_ru = chars.next()?;
    let second_ru = chars.next()?;
    if !same_letter_ignore_case(first_ru, second_ru) {
        return None;
    }

    let mut candidate = String::new();
    candidate.push(first_ru);
    candidate.extend(chars);
    if candidate == all_ru || candidate == map_original_events(word) {
        return None;
    }
    is_known_cyrillic_hyphenated_word(&candidate).then_some(candidate)
}

fn same_letter_ignore_case(left: char, right: char) -> bool {
    left.to_lowercase().to_string() == right.to_lowercase().to_string()
}

fn is_known_cyrillic_hyphenated_word(word: &str) -> bool {
    if !is_cyrillic_word(word) {
        return false;
    }
    let dict = russian_short_dictionary();
    word.split('-')
        .all(|part| part.chars().count() >= 3 && is_known_cyrillic_hyphen_part(part, dict))
}

fn is_known_cyrillic_hyphen_part(part: &str, dict: &HashSet<String>) -> bool {
    let lower = part.to_lowercase();
    dict.contains(&lower)
        || russian_generated_form_dictionary().contains(&lower)
        || is_known_short_accusative_a_form(&lower, dict)
}

fn is_known_short_accusative_a_form(word: &str, dict: &HashSet<String>) -> bool {
    let Some(stem) = word.strip_suffix('у') else {
        return false;
    };
    if stem.chars().count() < 2 {
        return false;
    }
    let lemma = format!("{stem}а");
    dict.contains(&lemma)
}

fn is_known_russian_word_or_form(word: &str) -> bool {
    russian_dictionary().contains(word) || russian_generated_form_dictionary().contains(word)
}

fn mixed_visual_latin_word_target_layout(word: &[KeyEvent]) -> Option<bool> {
    if word.is_empty()
        || word
            .iter()
            .any(|event| event.keycode == KeyCode::KEY_SPACE.code())
    {
        return None;
    }

    let first_layout = word.first()?.layout_is_ru;
    if word.iter().all(|event| event.layout_is_ru == first_layout) {
        return None;
    }

    let mut latin_count = 0usize;
    let mut same_key_homoglyph_count = 0usize;
    let mut other_cyrillic_count = 0usize;

    for event in word {
        let ch = original_event_char(event)?;
        if ch.is_ascii_alphabetic() {
            latin_count += 1;
        } else if is_cyrillic_letter(ch) {
            if same_key_latin_cyrillic_homoglyph(event) {
                same_key_homoglyph_count += 1;
            } else {
                other_cyrillic_count += 1;
            }
        }
    }

    if latin_count >= 2 && same_key_homoglyph_count > 0 && other_cyrillic_count == 0 {
        Some(true)
    } else {
        None
    }
}

fn original_event_char(event: &KeyEvent) -> Option<char> {
    if event.layout_is_ru {
        keycode_to_ru_char(event.keycode, event.shift)
    } else {
        keycode_to_us_char(event.keycode, event.shift)
    }
}

fn same_key_latin_cyrillic_homoglyph(event: &KeyEvent) -> bool {
    matches!(
        (
            keycode_to_us_char(event.keycode, event.shift),
            keycode_to_ru_char(event.keycode, event.shift),
        ),
        (Some('c' | 'C'), Some('с' | 'С'))
    )
}

fn is_cyrillic_letter(ch: char) -> bool {
    matches!(ch, 'А'..='я' | 'ё' | 'Ё')
}

fn normalize_mixed_word_to_last_layout(word: &[KeyEvent]) -> Option<String> {
    let target_is_ru = word.last()?.layout_is_ru;
    if word.iter().all(|event| event.layout_is_ru == target_is_ru) {
        return None;
    }

    let mut out = String::new();
    let mut run_start = 0;
    let mut current_layout = word.first()?.layout_is_ru;
    for (idx, event) in word.iter().enumerate() {
        if event.layout_is_ru != current_layout {
            let run = map_events_to_layout(&word[run_start..idx], target_is_ru);
            push_with_overlap(&mut out, &run);
            run_start = idx;
            current_layout = event.layout_is_ru;
        }
    }
    let run = map_events_to_layout(&word[run_start..], target_is_ru);
    push_with_overlap(&mut out, &run);

    (!out.is_empty()).then_some(out)
}

fn push_with_overlap(out: &mut String, next: &str) {
    if out.is_empty() || next.is_empty() {
        out.push_str(next);
        return;
    }

    let out_chars: Vec<char> = out.chars().collect();
    let next_chars: Vec<char> = next.chars().collect();
    let max_overlap = out_chars.len().min(next_chars.len());
    let overlap = (1..=max_overlap)
        .rev()
        .find(|len| {
            out_chars[out_chars.len() - len..]
                .iter()
                .zip(&next_chars[..*len])
                .all(|(left, right)| left == right)
        })
        .unwrap_or(0);
    out.push_str(&next_chars[overlap..].iter().collect::<String>());
}

fn plan_text_replacement(original: &str, replacement: &str) -> Option<TextReplacement> {
    if original == replacement {
        return None;
    }

    let original_chars: Vec<char> = original.chars().collect();
    let replacement_chars: Vec<char> = replacement.chars().collect();

    let mut prefix = 0;
    while prefix < original_chars.len()
        && prefix < replacement_chars.len()
        && original_chars[prefix] == replacement_chars[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < original_chars.len().saturating_sub(prefix)
        && suffix < replacement_chars.len().saturating_sub(prefix)
        && original_chars[original_chars.len() - 1 - suffix]
            == replacement_chars[replacement_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let original_end = original_chars.len() - suffix;
    let replacement_end = replacement_chars.len() - suffix;
    let backspaces = original_end.saturating_sub(prefix) as u32;
    let insert: String = replacement_chars[prefix..replacement_end].iter().collect();

    if backspaces == 0 && insert.is_empty() {
        return None;
    }

    Some(TextReplacement {
        move_left: suffix as u32,
        backspaces,
        insert,
        move_right: suffix as u32,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReplayLayoutDecision {
    target_is_ru: bool,
    mixed_layouts: bool,
}

fn replay_layout_decision(events: &[KeyEvent]) -> ReplayLayoutDecision {
    let typed_layouts: Vec<bool> = events
        .iter()
        .filter(|ev| is_layout_decision_key(KeyCode::new(ev.keycode)))
        .map(|ev| ev.layout_is_ru)
        .collect();
    let first_layout = typed_layouts.first().copied().unwrap_or(false);
    let last_layout = typed_layouts.last().copied().unwrap_or(first_layout);
    let mixed_layouts = typed_layouts.iter().any(|layout| *layout != first_layout);
    let target_is_ru = if mixed_layouts {
        mixed_visual_latin_word_target_layout(events).unwrap_or(last_layout)
    } else {
        !first_layout
    };
    ReplayLayoutDecision {
        target_is_ru,
        mixed_layouts,
    }
}

fn is_layout_decision_key(key: KeyCode) -> bool {
    is_typing_key(key) && key != KeyCode::KEY_SPACE
}

fn best_unique_ngram_candidate<I>(original: &str, candidates: I, min_margin: f64) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    let lower = original.to_lowercase();
    let mut best: Option<(String, f64)> = None;
    let mut second_best = f64::NEG_INFINITY;

    for candidate in candidates {
        if candidate == lower || !is_cyrillic_word(&candidate) {
            continue;
        }
        let margin = lay::ngram::ru_candidate_margin(&candidate, &lower);
        if margin < min_margin {
            continue;
        }

        match &best {
            Some((_, best_margin)) if margin <= *best_margin => {
                second_best = second_best.max(margin);
            }
            Some((_, best_margin)) => {
                second_best = second_best.max(*best_margin);
                best = Some((candidate, margin));
            }
            None => best = Some((candidate, margin)),
        }
    }

    let (candidate, best_margin) = best?;
    if best_margin - second_best < 0.40 {
        return None;
    }
    Some(apply_word_case(original, &candidate))
}

fn best_unique_dictionary_candidate<I>(
    original: &str,
    candidates: I,
    min_margin: f64,
) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    let lower = original.to_lowercase();
    let dict = russian_dictionary();
    let mut found: Option<String> = None;

    for candidate in candidates {
        if candidate == lower || !dict.contains(&candidate) {
            continue;
        }
        if lay::ngram::ru_candidate_margin(&candidate, &lower) < min_margin {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(candidate);
    }

    found.map(|candidate| apply_word_case(original, &candidate))
}

fn generate_missing_letter_candidates(lower: &str) -> impl Iterator<Item = String> + '_ {
    let chars: Vec<char> = lower.chars().collect();
    (0..=chars.len()).flat_map(move |idx| {
        RU_ALPHABET.into_iter().map({
            let chars = chars.clone();
            move |inserted| {
                let mut candidate = String::with_capacity(lower.len() + inserted.len_utf8());
                candidate.extend(chars[..idx].iter());
                candidate.push(inserted);
                candidate.extend(chars[idx..].iter());
                candidate
            }
        })
    })
}

fn generate_hard_sign_candidates(lower: &str) -> impl Iterator<Item = String> + '_ {
    let chars: Vec<char> = lower.chars().collect();
    (0..chars.len().saturating_sub(1)).filter_map(move |idx| {
        if chars[idx] != 'ь' || !matches!(chars[idx + 1], 'е' | 'ё' | 'ю' | 'я') {
            return None;
        }
        let mut candidate = chars.clone();
        candidate[idx] = 'ъ';
        Some(candidate.into_iter().collect())
    })
}

fn split_word_punctuation(token: &str) -> (&str, &str, &str) {
    let start = token
        .char_indices()
        .find(|(_, ch)| ch.is_alphanumeric())
        .map(|(idx, _)| idx)
        .unwrap_or(token.len());
    let end = token
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_alphanumeric())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(start);

    (&token[..start], &token[start..end], &token[end..])
}

fn is_cyrillic_word(word: &str) -> bool {
    word.chars()
        .all(|ch| matches!(ch, 'А'..='я' | 'ё' | 'Ё' | '-'))
}

fn apply_phrase_case(original: &str, replacement_lower: &str) -> String {
    if original.chars().next().is_some_and(|ch| ch.is_uppercase()) {
        capitalize_first(replacement_lower)
    } else {
        replacement_lower.to_string()
    }
}

fn apply_word_case(original: &str, replacement_lower: &str) -> String {
    if original
        .chars()
        .all(|ch| !ch.is_alphabetic() || ch.is_uppercase())
    {
        replacement_lower.to_uppercase()
    } else if original.chars().next().is_some_and(|ch| ch.is_uppercase()) {
        capitalize_first(replacement_lower)
    } else {
        replacement_lower.to_string()
    }
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::new();
    out.extend(first.to_uppercase());
    out.push_str(chars.as_str());
    out
}

fn russian_dictionary() -> &'static HashSet<String> {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let mut words = load_hunspell_words_min_len("/usr/share/hunspell/ru_RU.dic", 5)
            .unwrap_or_else(|e| {
                log(&format!("⚠ ru dictionary load failed: {e}"));
                HashSet::new()
            });
        if let Some(home) = std::env::var_os("HOME") {
            let path = std::path::PathBuf::from(home).join(PROTECTED_WORDS_PATH);
            if let Ok(custom) = load_word_list(&path) {
                words.extend(custom);
            }
        }
        #[cfg(test)]
        words.extend(test_russian_forms().into_iter().map(str::to_string));
        words
    })
}

fn russian_short_dictionary() -> &'static HashSet<String> {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let words =
            load_hunspell_words_min_len("/usr/share/hunspell/ru_RU.dic", 3).unwrap_or_else(|e| {
                log(&format!("⚠ short ru dictionary load failed: {e}"));
                HashSet::new()
            });
        #[cfg(test)]
        {
            let mut words = words;
            words.extend(test_russian_forms().into_iter().map(str::to_string));
            words.insert("пара".to_string());
            words
        }
        #[cfg(not(test))]
        {
            words
        }
    })
}

fn russian_tiny_dictionary() -> &'static HashSet<String> {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let words =
            load_hunspell_words_min_len("/usr/share/hunspell/ru_RU.dic", 2).unwrap_or_else(|e| {
                log(&format!("⚠ tiny ru dictionary load failed: {e}"));
                HashSet::new()
            });
        #[cfg(test)]
        {
            let mut words = words;
            words.extend(test_russian_forms().into_iter().map(str::to_string));
            words.insert("не".to_string());
            words
        }
        #[cfg(not(test))]
        {
            words
        }
    })
}

fn russian_generated_form_dictionary() -> &'static HashSet<String> {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        load_hunspell_generated_forms_min_len(
            "/usr/share/hunspell/ru_RU.dic",
            "/usr/share/hunspell/ru_RU.aff",
            5,
        )
        .unwrap_or_else(|e| {
            log(&format!("⚠ ru generated forms load failed: {e}"));
            HashSet::new()
        })
    })
}

#[cfg(test)]
fn test_russian_forms() -> [&'static str; 16] {
    [
        "библиотеку",
        "приблизительные",
        "привет",
        "проверка",
        "работает",
        "расчеты",
        "нормально",
        "ошибка",
        "ошибся",
        "явно",
        "исправлено",
        "исправляет",
        "ладно",
        "можно",
        "дальше",
        "правильно",
    ]
}

fn load_hunspell_words_min_len(path: &str, min_chars: usize) -> std::io::Result<HashSet<String>> {
    let text = std::fs::read_to_string(path)?;
    let mut words = HashSet::new();
    for line in text.lines().skip(1) {
        let word = line.split('/').next().unwrap_or("").trim();
        if word.chars().count() >= min_chars && is_cyrillic_word(word) {
            words.insert(word.to_lowercase());
        }
    }
    Ok(words)
}

struct HunspellSuffixRule {
    strip: String,
    add: String,
    condition: String,
}

fn load_hunspell_generated_forms_min_len(
    dic_path: &str,
    aff_path: &str,
    min_chars: usize,
) -> std::io::Result<HashSet<String>> {
    let rules = load_simple_hunspell_suffix_rules(aff_path)?;
    let text = std::fs::read_to_string(dic_path)?;
    let mut forms = HashSet::new();

    for line in text.lines().skip(1) {
        let line = line.trim();
        let Some((word, flags)) = line.split_once('/') else {
            continue;
        };
        let word = word.trim().to_lowercase();
        if word.is_empty() {
            continue;
        }
        let flags = flags.split_whitespace().next().unwrap_or("");
        for flag in flags.chars() {
            let Some(flag_rules) = rules.get(&flag) else {
                continue;
            };
            for rule in flag_rules {
                if !hunspell_condition_matches(&word, &rule.condition) {
                    continue;
                }
                let stem = if rule.strip == "0" {
                    word.as_str()
                } else if let Some(stem) = word.strip_suffix(&rule.strip) {
                    stem
                } else {
                    continue;
                };
                let candidate = if rule.add == "0" {
                    stem.to_string()
                } else {
                    format!("{stem}{}", rule.add)
                };
                if candidate.chars().count() >= min_chars && is_cyrillic_word(&candidate) {
                    forms.insert(candidate);
                }
            }
        }
    }

    Ok(forms)
}

fn load_simple_hunspell_suffix_rules(
    path: &str,
) -> std::io::Result<HashMap<char, Vec<HunspellSuffixRule>>> {
    let text = std::fs::read_to_string(path)?;
    let mut rules: HashMap<char, Vec<HunspellSuffixRule>> = HashMap::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 || parts[0] != "SFX" || parts[3].parse::<usize>().is_ok() {
            continue;
        }
        let Some(flag) = parts[1].chars().next() else {
            continue;
        };
        let condition = parts[4];
        if !is_simple_hunspell_suffix_condition(condition) {
            continue;
        }
        rules.entry(flag).or_default().push(HunspellSuffixRule {
            strip: parts[2].to_string(),
            add: parts[3].split('/').next().unwrap_or(parts[3]).to_string(),
            condition: condition.to_string(),
        });
    }

    Ok(rules)
}

fn is_simple_hunspell_suffix_condition(condition: &str) -> bool {
    condition == "." || condition.chars().all(is_cyrillic_letter)
}

fn hunspell_condition_matches(word: &str, condition: &str) -> bool {
    condition == "." || word.ends_with(condition)
}

fn load_word_list(path: &std::path::Path) -> std::io::Result<HashSet<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_lowercase)
        .collect())
}

fn replace_visual_b_in_context(original: &str, target: &str) -> Option<String> {
    if !contains_visual_b_word(original) {
        return None;
    }

    let base = if has_cyrillic_text(original) {
        original
    } else {
        target
    };
    replace_visual_b_words(original, base)
}

fn replace_visual_b_words(original: &str, base: &str) -> Option<String> {
    let original_segments = split_ws_segments(original);
    let base_segments = split_ws_segments(base);
    if original_segments.len() != base_segments.len() {
        return None;
    }

    let mut changed = false;
    let mut out = String::with_capacity(base.len());
    for ((orig, orig_ws), (base_part, base_ws)) in
        original_segments.iter().zip(base_segments.iter())
    {
        if orig_ws != base_ws {
            return None;
        }
        if *orig_ws {
            out.push_str(base_part);
            continue;
        }

        let replacement = match *orig {
            "b" => Some("в"),
            "B" => Some("В"),
            _ => None,
        };
        if let Some(replacement) = replacement {
            changed = true;
            out.push_str(replacement);
        } else {
            out.push_str(base_part);
        }
    }

    if changed {
        Some(out)
    } else {
        None
    }
}

fn split_ws_segments(text: &str) -> Vec<(&str, bool)> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut current_ws: Option<bool> = None;

    for (idx, ch) in text.char_indices() {
        let ws = ch.is_whitespace();
        match current_ws {
            Some(prev) if prev != ws => {
                segments.push((&text[start..idx], prev));
                start = idx;
                current_ws = Some(ws);
            }
            None => current_ws = Some(ws),
            _ => {}
        }
    }

    if let Some(ws) = current_ws {
        segments.push((&text[start..], ws));
    }
    segments
}

fn contains_visual_b_word(text: &str) -> bool {
    text.split_whitespace()
        .any(|word| word == "b" || word == "B")
}

fn has_cyrillic_text(text: &str) -> bool {
    text.chars().any(|ch| matches!(ch, 'А'..='я' | 'ё' | 'Ё'))
}

fn replacement_rules() -> &'static HashMap<String, String> {
    static RULES: OnceLock<HashMap<String, String>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = HashMap::new();
        #[cfg(test)]
        rules.extend(test_replacement_rules());

        if let Some(home) = std::env::var_os("HOME") {
            let path = std::path::PathBuf::from(home).join(REPLACEMENTS_PATH);
            if let Ok(text) = std::fs::read_to_string(path) {
                match serde_json::from_str::<HashMap<String, String>>(&text) {
                    Ok(custom) => rules.extend(custom),
                    Err(e) => log(&format!("⚠ replacements parse failed: {e}")),
                }
            }
        }

        rules
    })
}

#[cfg(test)]
fn test_replacement_rules() -> HashMap<String, String> {
    HashMap::from([
        ("подлючись".to_string(), "подключись".to_string()),
        ("надйи".to_string(), "найди".to_string()),
        ("нуда".to_string(), "ну да".to_string()),
        ("вчем".to_string(), "в чем".to_string()),
        ("можн".to_string(), "можно".to_string()),
        ("дльше".to_string(), "дальше".to_string()),
        ("дальг".to_string(), "дальше".to_string()),
        ("првильно".to_string(), "правильно".to_string()),
    ])
}

fn split_edge_whitespace(text: &str) -> (&str, &str, &str) {
    let start = text
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let end = text
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(start);

    (&text[..start], &text[start..end], &text[end..])
}

#[derive(serde::Serialize)]
struct LearningEntry<'a> {
    ts: u64,
    kind: &'a str,
    from: &'a str,
    to: &'a str,
    replace_words: usize,
    words: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    lay_kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lay_from: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lay_to: Option<&'a str>,
}

fn append_learning_log(kind: &str, from: &str, to: &str, replace_words: usize, words: usize) {
    if !active_learning_log() {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::PathBuf::from(home).join(LEARN_LOG_PATH);
    append_learning_log_to_path(&path, kind, from, to, replace_words, words);
}

fn append_user_correction_learning_log(correction: &UserLearningCorrection) {
    if !active_learning_log() {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let home = std::path::PathBuf::from(home);
    let path = home.join(LEARN_LOG_PATH);
    append_user_correction_learning_log_to_path(&path, correction);
    match promote_user_correction_if_repeated(
        &home.join(LEARN_CANDIDATES_PATH),
        &home.join(REPLACEMENTS_PATH),
        correction,
    ) {
        LearningPromotion::Promoted { from, to } => {
            log(&format!("  learn: promoted exact rule {from:?} → {to:?}"));
        }
        LearningPromotion::Recorded { count, from, to } => {
            log(&format!(
                "  learn: candidate {from:?} → {to:?}, count={count}/{LEARN_PROMOTION_THRESHOLD}"
            ));
        }
        LearningPromotion::Skipped => {}
    }
}

fn append_learning_log_to_path(
    path: &std::path::Path,
    kind: &str,
    from: &str,
    to: &str,
    replace_words: usize,
    words: usize,
) {
    let entry = LearningEntry {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        kind,
        from,
        to,
        replace_words,
        words,
        lay_kind: None,
        lay_from: None,
        lay_to: None,
    };
    append_learning_entry_to_path(path, &entry);
}

fn append_user_correction_learning_log_to_path(
    path: &std::path::Path,
    correction: &UserLearningCorrection,
) {
    let entry = LearningEntry {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        kind: "user-correction",
        from: &correction.from,
        to: &correction.to,
        replace_words: correction.replace_words,
        words: correction.words,
        lay_kind: Some(&correction.lay_kind),
        lay_from: Some(&correction.lay_from),
        lay_to: Some(&correction.lay_to),
    };
    append_learning_entry_to_path(path, &entry);
}

fn append_learning_entry_to_path(path: &std::path::Path, entry: &LearningEntry<'_>) {
    if entry.from == entry.to || entry.from.trim().is_empty() || entry.to.trim().is_empty() {
        return;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log(&format!("⚠ learn-log mkdir failed: {e}"));
            return;
        }
    }

    let Ok(mut line) = serde_json::to_string(&entry) else {
        return;
    };
    line.push('\n');

    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        Ok(mut f) => {
            if f.write_all(line.as_bytes()).is_ok() {
                compact_learning_log_if_needed(path);
                #[cfg(not(test))]
                lay::stats::record_learning_log_entry(entry.kind);
                log("  learn-log: correction saved");
            }
        }
        Err(e) => log(&format!("⚠ learn-log open failed: {e}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LearningPromotion {
    Skipped,
    Recorded {
        from: String,
        to: String,
        count: u32,
    },
    Promoted {
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct LearningCandidate {
    from: String,
    to: String,
    count: u32,
    first_ts: u64,
    last_ts: u64,
    promoted: bool,
}

fn promote_user_correction_if_repeated(
    candidates_path: &std::path::Path,
    replacements_path: &std::path::Path,
    correction: &UserLearningCorrection,
) -> LearningPromotion {
    let Some((from, to)) = normalizable_learning_rule(correction) else {
        return LearningPromotion::Skipped;
    };

    let now = unix_timestamp();
    let key = format!("{from}\u{1f}{to}");
    let mut candidates = load_learning_candidates(candidates_path);
    let candidate = candidates.entry(key).or_insert_with(|| LearningCandidate {
        from: from.clone(),
        to: to.clone(),
        count: 0,
        first_ts: now,
        last_ts: now,
        promoted: false,
    });
    candidate.count = candidate.count.saturating_add(1);
    candidate.last_ts = now;

    if candidate.promoted {
        remember_promoted_replacement(&from, &to);
        let _ = save_learning_candidates(candidates_path, &candidates);
        return LearningPromotion::Promoted { from, to };
    }

    if candidate.count < LEARN_PROMOTION_THRESHOLD {
        let count = candidate.count;
        let _ = save_learning_candidates(candidates_path, &candidates);
        return LearningPromotion::Recorded { from, to, count };
    }

    match add_replacement_rule_to_path(replacements_path, &from, &to) {
        Ok(true) | Ok(false) => {
            candidate.promoted = true;
            remember_promoted_replacement(&from, &to);
            #[cfg(not(test))]
            lay::stats::record_learning_promotion();
            let _ = save_learning_candidates(candidates_path, &candidates);
            LearningPromotion::Promoted { from, to }
        }
        Err(e) => {
            log(&format!("⚠ learn promotion failed: {e}"));
            let _ = save_learning_candidates(candidates_path, &candidates);
            LearningPromotion::Skipped
        }
    }
}

fn normalizable_learning_rule(correction: &UserLearningCorrection) -> Option<(String, String)> {
    if correction.lay_kind == "layout-replay" {
        return None;
    }

    let from = correction.from.trim();
    let to = correction.to.trim();
    if from.is_empty() || to.is_empty() || from == to {
        return None;
    }
    if from.split_whitespace().count() != 1 || to.split_whitespace().count() > 3 {
        return None;
    }

    let from_lower = from.to_lowercase();
    let to_lower = to.to_lowercase();
    let from_letters = from_lower.chars().filter(|ch| ch.is_alphabetic()).count();
    let to_letters = to_lower.chars().filter(|ch| ch.is_alphabetic()).count();
    if from_letters < 4 || to_letters < 2 {
        return None;
    }
    if !is_cyrillic_word(&from_lower) {
        return None;
    }
    if !to_lower
        .chars()
        .all(|ch| is_cyrillic_letter(ch) || ch.is_whitespace() || ch == '-')
    {
        return None;
    }
    if is_known_russian_word_or_form(&from_lower) {
        return None;
    }

    Some((from_lower, to_lower))
}

fn load_learning_candidates(path: &std::path::Path) -> BTreeMap<String, LearningCandidate> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_learning_candidates(
    path: &std::path::Path,
    candidates: &BTreeMap<String, LearningCandidate>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(candidates).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, format!("{text}\n"))
}

fn add_replacement_rule_to_path(
    path: &std::path::Path,
    from: &str,
    to: &str,
) -> Result<bool, String> {
    let mut rules: BTreeMap<String, String> = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();

    if let Some(existing) = rules.get(from) {
        if existing == to {
            return Ok(false);
        }
        return Err(format!(
            "replacement conflict for {from:?}: existing {existing:?}, learned {to:?}"
        ));
    }

    rules.insert(from.to_string(), to.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&rules).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{text}\n")).map_err(|e| e.to_string())?;
    Ok(true)
}

fn compact_learning_log_if_needed(path: &std::path::Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= LEARN_LOG_MAX_BYTES {
        return;
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let compacted = keep_last_jsonl_lines(&content, LEARN_LOG_KEEP_LINES);
    if std::fs::write(path, compacted).is_ok() {
        log("  learn-log: compacted");
    }
}

fn keep_last_jsonl_lines(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    let mut out = lines[start..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(key: KeyCode, layout_is_ru: bool) -> KeyEvent {
        KeyEvent {
            keycode: key.code(),
            shift: false,
            layout_is_ru,
        }
    }

    fn push_keys(buffer: &mut WordBuffer, keys: &[KeyCode], layout_is_ru: bool) {
        for key in keys {
            buffer.push(key_event(*key, layout_is_ru));
        }
    }

    fn key_events(keys: &[KeyCode], layout_is_ru: bool) -> Vec<KeyEvent> {
        keys.iter()
            .map(|key| key_event(*key, layout_is_ru))
            .collect()
    }

    fn ascii_hyphen_token_keycodes() -> [KeyCode; 5] {
        [
            KeyCode::KEY_W,
            KeyCode::KEY_I,
            KeyCode::KEY_MINUS,
            KeyCode::KEY_F,
            KeyCode::KEY_I,
        ]
    }

    #[test]
    fn text_insert_runs_use_uinput_layout_channels() {
        let runs = text_to_uinput_runs("Привет Double", true).expect("typable text");
        assert_eq!(runs.len(), 2);
        assert!(runs[0].target_is_ru);
        assert!(!runs[1].target_is_ru);
        assert_eq!(map_events_to_layout(&runs[0].events, true), "Привет ");
        assert_eq!(map_events_to_layout(&runs[1].events, false), "Double");

        let runs = text_to_uinput_runs("ну да ", true).expect("typable text");
        assert_eq!(runs.len(), 1);
        assert!(runs[0].target_is_ru);
        assert_eq!(map_events_to_layout(&runs[0].events, true), "ну да ");

        let runs = text_to_uinput_runs("hello world", false).expect("typable text");
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].target_is_ru);
        assert_eq!(map_events_to_layout(&runs[0].events, false), "hello world");

        assert!(text_to_uinput_runs("привет 🙂", true).is_none());
    }

    #[test]
    fn typing_assist_minimal_plan_keeps_inter_word_space() {
        let plan = plan_text_replacement("чтобы точнр ", "чтобы точно ").expect("replacement");

        assert_eq!(plan.move_left, 1);
        assert_eq!(plan.backspaces, 1);
        assert_eq!(plan.insert, "о");
        assert_eq!(plan.move_right, 1);
    }

    fn push_key_events(buffer: &mut WordBuffer, keys: &[(KeyCode, bool)], layout_is_ru: bool) {
        for (key, shift) in keys {
            buffer.push(KeyEvent {
                keycode: key.code(),
                shift: *shift,
                layout_is_ru,
            });
        }
    }

    fn text_key_event(ch: char, layout_is_ru: bool) -> KeyEvent {
        const KEYS: &[KeyCode] = &[
            KeyCode::KEY_A,
            KeyCode::KEY_B,
            KeyCode::KEY_C,
            KeyCode::KEY_D,
            KeyCode::KEY_E,
            KeyCode::KEY_F,
            KeyCode::KEY_G,
            KeyCode::KEY_H,
            KeyCode::KEY_I,
            KeyCode::KEY_J,
            KeyCode::KEY_K,
            KeyCode::KEY_L,
            KeyCode::KEY_M,
            KeyCode::KEY_N,
            KeyCode::KEY_O,
            KeyCode::KEY_P,
            KeyCode::KEY_Q,
            KeyCode::KEY_R,
            KeyCode::KEY_S,
            KeyCode::KEY_T,
            KeyCode::KEY_U,
            KeyCode::KEY_V,
            KeyCode::KEY_W,
            KeyCode::KEY_X,
            KeyCode::KEY_Y,
            KeyCode::KEY_Z,
            KeyCode::KEY_SEMICOLON,
            KeyCode::KEY_APOSTROPHE,
            KeyCode::KEY_COMMA,
            KeyCode::KEY_DOT,
            KeyCode::KEY_LEFTBRACE,
            KeyCode::KEY_RIGHTBRACE,
            KeyCode::KEY_GRAVE,
            KeyCode::KEY_SLASH,
            KeyCode::KEY_MINUS,
        ];

        for key in KEYS {
            for shift in [false, true] {
                let mapped = if layout_is_ru {
                    keycode_to_ru_char(key.code(), shift)
                } else {
                    keycode_to_us_char(key.code(), shift)
                };
                if mapped == Some(ch) {
                    return KeyEvent {
                        keycode: key.code(),
                        shift,
                        layout_is_ru,
                    };
                }
            }
        }

        panic!("no key event for {ch:?} in layout_is_ru={layout_is_ru}");
    }

    fn push_text_as_layout(buffer: &mut WordBuffer, text: &str, layout_is_ru: bool) {
        for ch in text.chars() {
            if ch == ' ' {
                buffer.handle_space();
            } else {
                buffer.push(text_key_event(ch, layout_is_ru));
            }
        }
    }

    fn assert_smart_pair(
        left: &str,
        left_layout_is_ru: bool,
        current_typed: &str,
        current_layout_is_ru: bool,
        expected: &str,
    ) {
        let mut buffer = WordBuffer::new();
        push_text_as_layout(&mut buffer, left, left_layout_is_ru);
        buffer.handle_space();
        push_text_as_layout(&mut buffer, current_typed, current_layout_is_ru);
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let original = map_original_events(&events);
        let got = decide_scoped_tail_correction(&events).unwrap_or(original.clone());

        assert_eq!(got, expected, "original tail: {original:?}");
    }

    fn map_target_events(events: &[KeyEvent], target_is_ru: bool) -> String {
        events
            .iter()
            .filter_map(|ev| {
                if target_is_ru {
                    keycode_to_ru_char(ev.keycode, ev.shift)
                } else {
                    keycode_to_us_char(ev.keycode, ev.shift)
                }
            })
            .collect()
    }

    fn apply_typing_assist_to_text_tail(text: &str) -> Option<String> {
        apply_typing_assist_exact(text).or_else(|| {
            let (leading, core, trailing) = split_edge_whitespace(text);
            let segments = split_ws_segments(core);
            if segments.len() < 3 {
                return None;
            }

            let mut suffix_start = core.len();
            let mut non_ws_seen = 0;
            for (segment, is_ws) in segments.iter().rev() {
                suffix_start -= segment.len();
                if !is_ws {
                    non_ws_seen += 1;
                    if non_ws_seen == 2 {
                        break;
                    }
                }
            }

            let prefix = &core[..suffix_start];
            let suffix = &core[suffix_start..];
            let replacement = apply_typing_assist_exact(&format!("{suffix}{trailing}"))?;
            Some(format!("{leading}{prefix}{replacement}"))
        })
    }

    #[test]
    fn parses_gdbus_string_tuple() {
        assert_eq!(parse_gdbus_string("('us',)"), Some("us".to_string()));
    }

    #[test]
    fn parses_current_layout_from_list_layouts_reply() {
        assert_eq!(
            parse_current_layout_from_list("('0:xkb:us,1:xkb:ru*',)"),
            Some("ru".to_string())
        );
    }

    #[test]
    fn marks_current_word_after_replay_for_next_toggle() {
        let mut buffer = WordBuffer::new();
        for key in [
            KeyCode::KEY_D,
            KeyCode::KEY_H,
            KeyCode::KEY_T,
            KeyCode::KEY_V,
            KeyCode::KEY_Z,
        ] {
            buffer.push(KeyEvent {
                keycode: key.code(),
                shift: false,
                layout_is_ru: false,
            });
        }

        buffer.mark_replayed_layout(1, true);
        let (events, _) = buffer.what_to_replay(1).expect("word is buffered");

        assert!(events.iter().all(|event| event.layout_is_ru));
        assert!(buffer.replay_toggle_ready());
    }

    #[test]
    fn short_fragments_force_replay_without_llm() {
        assert!(should_force_replay_for_short_fragment("N"));
        assert!(should_force_replay_for_short_fragment("gh"));
        assert!(should_force_replay_for_short_fragment("т"));
        assert!(!should_force_replay_for_short_fragment("ghb"));
        assert!(!should_force_replay_for_short_fragment("a b"));
        assert!(!should_force_replay_for_short_fragment(""));
    }

    #[test]
    fn config_replace_words_is_independent_from_engine_mode() {
        let simple = LayConfig {
            mode: "simple".to_string(),
            correction_engine: Some("replay".to_string()),
            replace_words: 2,
            ..LayConfig::default()
        };
        let smart = LayConfig {
            mode: "simple".to_string(),
            correction_engine: Some("smart".to_string()),
            replace_words: 2,
            ..LayConfig::default()
        };

        assert_eq!(simple.active_replace_words(), 2);
        assert_eq!(smart.active_replace_words(), 2);
        assert_eq!(simple.active_correction_engine(), CorrectionEngine::Replay);
        assert_eq!(smart.active_correction_engine(), CorrectionEngine::Smart);
    }

    #[test]
    fn auto_switch_layout_is_enabled_by_default() {
        assert!(LayConfig::default().auto_switch_layout);
    }

    #[test]
    fn legacy_llm_mode_maps_to_smart_only_without_explicit_engine() {
        let legacy = LayConfig {
            mode: "llm".to_string(),
            correction_engine: None,
            ..LayConfig::default()
        };
        let explicit_replay = LayConfig {
            mode: "llm".to_string(),
            correction_engine: Some("replay".to_string()),
            ..LayConfig::default()
        };

        assert_eq!(legacy.active_correction_engine(), CorrectionEngine::Smart);
        assert_eq!(
            explicit_replay.active_correction_engine(),
            CorrectionEngine::Replay
        );
    }

    #[test]
    fn two_word_replay_keeps_space_and_backspace_count() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_G,
                KeyCode::KEY_H,
                KeyCode::KEY_B,
                KeyCode::KEY_D,
                KeyCode::KEY_T,
                KeyCode::KEY_N,
            ],
            false,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[KeyCode::KEY_V, KeyCode::KEY_B, KeyCode::KEY_H],
            false,
        );

        let (events, backspaces) = buffer.what_to_replay(2).expect("two words are buffered");

        assert_eq!(map_original_events(&events), "ghbdtn vbh");
        assert_eq!(backspaces, 10);
        assert_eq!(events[6].keycode, KeyCode::KEY_SPACE.code());
        let decision = replay_layout_decision(&events);
        assert_eq!(
            map_target_events(&events, decision.target_is_ru),
            "привет мир"
        );
    }

    #[test]
    fn two_word_trailing_space_replay_deletes_expected_tail() {
        let mut buffer = WordBuffer::new();
        push_keys(&mut buffer, &[KeyCode::KEY_G, KeyCode::KEY_H], false);
        buffer.handle_space();
        push_keys(&mut buffer, &[KeyCode::KEY_V, KeyCode::KEY_B], false);
        buffer.handle_space();

        let (events, backspaces) = buffer.what_to_replay(2).expect("two completed words");

        assert_eq!(map_original_events(&events), "gh vb ");
        assert_eq!(backspaces, 6);
    }

    #[test]
    fn smart_scope_after_trailing_space_uses_last_completed_word_only() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_R,
                KeyCode::KEY_J,
                KeyCode::KEY_H,
                KeyCode::KEY_J,
                KeyCode::KEY_X,
                KeyCode::KEY_T,
            ],
            true,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[KeyCode::KEY_N, KeyCode::KEY_F, KeyCode::KEY_V],
            true,
        );
        buffer.handle_space();

        let scope = effective_replace_words(&buffer, 2, CorrectionEngine::Smart, true);
        let (events, backspaces) = buffer.what_to_replay(scope).expect("last word is buffered");

        assert_eq!(scope, 1);
        assert_eq!(map_original_events(&events), "там ");
        assert_eq!(backspaces, 4);
    }

    #[test]
    fn replay_layout_decision_ignores_inserted_space() {
        let events = [
            key_event(KeyCode::KEY_G, true),
            key_event(KeyCode::KEY_H, true),
            key_event(KeyCode::KEY_SPACE, false),
            key_event(KeyCode::KEY_V, true),
            key_event(KeyCode::KEY_B, true),
        ];

        assert!(!is_layout_decision_key(KeyCode::KEY_SPACE));
        assert_eq!(
            replay_layout_decision(&events),
            ReplayLayoutDecision {
                target_is_ru: false,
                mixed_layouts: false,
            }
        );
    }

    #[test]
    fn shortcut_modified_text_keys_do_not_enter_word_buffer() {
        let mut modifiers = ShiftState::default();

        modifiers.update(KeyCode::KEY_LEFTCTRL, 1);
        assert!(should_ignore_buffer_key(
            KeyCode::KEY_EQUAL,
            &modifiers,
            true
        ));
        assert!(should_ignore_buffer_key(
            KeyCode::KEY_MINUS,
            &modifiers,
            true
        ));
        assert!(should_ignore_buffer_key(
            KeyCode::KEY_SPACE,
            &modifiers,
            true
        ));
        assert!(should_ignore_buffer_key(KeyCode::KEY_A, &modifiers, true));

        modifiers.update(KeyCode::KEY_LEFTCTRL, 0);
        assert!(!should_ignore_buffer_key(KeyCode::KEY_A, &modifiers, true));
    }

    #[test]
    fn leading_plus_minus_symbols_do_not_attach_to_next_word() {
        let mut buffer = WordBuffer::new();

        for (key, shift) in [
            (KeyCode::KEY_EQUAL, true),
            (KeyCode::KEY_EQUAL, false),
            (KeyCode::KEY_MINUS, true),
            (KeyCode::KEY_MINUS, false),
        ] {
            let mut modifiers = ShiftState::default();
            modifiers.update(KeyCode::KEY_LEFTSHIFT, i32::from(shift));
            if !should_ignore_buffer_key(key, &modifiers, buffer.current.is_empty()) {
                buffer.push(key_event(key, shift));
            }
        }
        push_text_as_layout(&mut buffer, "есть", true);

        let (events, backspaces) = buffer.what_to_replay(1).expect("word tail");

        assert_eq!(map_original_events(&events), "есть");
        assert_eq!(backspaces, 4);
    }

    #[test]
    fn visual_latin_word_with_cyrillic_c_homoglyph_replays_to_ru() {
        let events = [
            key_event(KeyCode::KEY_C, true),
            key_event(KeyCode::KEY_H, false),
            key_event(KeyCode::KEY_E, false),
            key_event(KeyCode::KEY_C, false),
        ];

        assert_eq!(map_original_events(&events), "сhec");
        assert_eq!(
            replay_layout_decision(&events),
            ReplayLayoutDecision {
                target_is_ru: true,
                mixed_layouts: true,
            }
        );
        assert_eq!(map_target_events(&events, true), "срус");
    }

    #[test]
    fn smart_decision_keeps_good_word_and_converts_bad_neighbor() {
        assert_eq!(
            decide_correction("Главное Вщгиду", "Ukfdyjt Double", CorrectionEngine::Smart),
            Correction::InsertText("Главное Double".to_string())
        );
    }

    #[test]
    fn scoped_tail_keeps_good_previous_word_and_flips_current_fragment() {
        let mut buffer = WordBuffer::new();
        push_keys(&mut buffer, &[KeyCode::KEY_D], true);
        buffer.handle_space();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_D, true),
                (KeyCode::KEY_O, false),
                (KeyCode::KEY_U, false),
                (KeyCode::KEY_B, false),
                (KeyCode::KEY_L, false),
                (KeyCode::KEY_E, false),
            ],
            true,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(map_original_events(&events), "в Вщгиду");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("в Double".to_string())
        );
        assert_eq!(
            plan_text_replacement("в Вщгиду", "в Double"),
            Some(TextReplacement {
                move_left: 0,
                backspaces: 6,
                insert: "Double".to_string(),
                move_right: 0,
            })
        );
    }

    #[test]
    fn smart_insert_remembers_only_inserted_tail_for_immediate_undo() {
        let mut buffer = WordBuffer::new();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_G, true),
                (KeyCode::KEY_H, false),
                (KeyCode::KEY_J, false),
                (KeyCode::KEY_D, false),
                (KeyCode::KEY_T, false),
                (KeyCode::KEY_H, false),
                (KeyCode::KEY_R, false),
                (KeyCode::KEY_F, false),
            ],
            true,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_C,
                KeyCode::KEY_K,
                KeyCode::KEY_J,
                KeyCode::KEY_D,
                KeyCode::KEY_F,
            ],
            true,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let original = map_original_events(&events);
        let replacement = decide_scoped_tail_correction(&events).expect("smart replacement");
        let plan = plan_text_replacement(&original, &replacement).expect("minimal plan");

        assert_eq!(original, "Проверка слова");
        assert_eq!(replacement, "Проверка ckjdf");
        assert_eq!(
            plan,
            TextReplacement {
                move_left: 0,
                backspaces: 5,
                insert: "ckjdf".to_string(),
                move_right: 0,
            }
        );
        assert!(buffer.remember_inserted_tail_for_replay(&events, &plan, false));

        let (undo_events, undo_backspaces) = buffer.what_to_replay(2).expect("undo tail");
        let undo_decision = replay_layout_decision(&undo_events);
        assert_eq!(map_original_events(&undo_events), "ckjdf");
        assert_eq!(undo_backspaces, 5);
        assert!(undo_decision.target_is_ru);
        assert_eq!(map_events_to_layout(&undo_events, true), "слова");
        assert!(buffer.replay_toggle_ready());
    }

    #[test]
    fn smart_insert_remembers_last_word_after_full_tail_replace() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_G,
                KeyCode::KEY_O,
                KeyCode::KEY_O,
                KeyCode::KEY_D,
            ],
            true,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_N,
                KeyCode::KEY_T,
                KeyCode::KEY_R,
                KeyCode::KEY_C,
                KeyCode::KEY_N,
            ],
            true,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let original = map_original_events(&events);
        let replacement = decide_scoped_tail_correction(&events).expect("smart replacement");
        let plan = plan_text_replacement(&original, &replacement).expect("minimal plan");

        assert_eq!(original, "пщщв текст");
        assert_eq!(replacement, "good ntrcn");
        assert_eq!(
            plan,
            TextReplacement {
                move_left: 0,
                backspaces: 10,
                insert: "good ntrcn".to_string(),
                move_right: 0,
            }
        );
        assert!(!buffer.remember_inserted_tail_for_replay(&events, &plan, false));
        assert!(buffer.remember_inserted_last_word_for_replay(&events, &plan));

        let (undo_events, undo_backspaces) = buffer.what_to_replay(2).expect("undo tail");
        let undo_decision = replay_layout_decision(&undo_events);
        assert_eq!(map_original_events(&undo_events), "ntrcn");
        assert_eq!(undo_backspaces, 5);
        assert!(undo_decision.target_is_ru);
        assert_eq!(map_events_to_layout(&undo_events, true), "текст");
        assert!(buffer.replay_toggle_ready());
    }

    #[test]
    fn scoped_tail_keeps_good_english_previous_word_and_flips_current_layout_word() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_G,
                KeyCode::KEY_O,
                KeyCode::KEY_O,
                KeyCode::KEY_D,
            ],
            false,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_N,
                KeyCode::KEY_T,
                KeyCode::KEY_R,
                KeyCode::KEY_C,
                KeyCode::KEY_N,
            ],
            false,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(map_original_events(&events), "good ntrcn");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("good текст".to_string())
        );
    }

    #[test]
    fn scoped_tail_keeps_completed_ascii_title_word_and_flips_current_latin_keys() {
        let mut buffer = WordBuffer::new();
        let left_events = [
            KeyEvent {
                keycode: KeyCode::KEY_D.code(),
                shift: true,
                layout_is_ru: false,
            },
            key_event(KeyCode::KEY_O, false),
            key_event(KeyCode::KEY_U, false),
            key_event(KeyCode::KEY_B, false),
            key_event(KeyCode::KEY_L, false),
            key_event(KeyCode::KEY_E, false),
        ];
        for event in left_events {
            buffer.push(event);
        }
        buffer.handle_space();
        let current_events = [
            key_event(KeyCode::KEY_N, false),
            key_event(KeyCode::KEY_J, false),
            key_event(KeyCode::KEY_SEMICOLON, false),
            key_event(KeyCode::KEY_T, false),
        ];
        for event in current_events {
            buffer.push(event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let left = map_original_events(&left_events);
        let current_original = map_original_events(&current_events);
        let current_target = map_events_to_layout(&current_events, true);

        assert_eq!(
            map_original_events(&events),
            format!("{left} {current_original}")
        );
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some(format!("{left} {current_target}"))
        );
        assert_eq!(
            plan_text_replacement(
                &format!("{left} {current_original}"),
                &format!("{left} {current_target}")
            ),
            Some(TextReplacement {
                move_left: 0,
                backspaces: current_original.chars().count() as u32,
                insert: current_target,
                move_right: 0,
            })
        );
    }

    #[test]
    fn scoped_tail_trailing_space_flips_only_current_completed_latin_keys() {
        let mut buffer = WordBuffer::new();
        let left_events = [
            KeyEvent {
                keycode: KeyCode::KEY_D.code(),
                shift: true,
                layout_is_ru: false,
            },
            key_event(KeyCode::KEY_O, false),
            key_event(KeyCode::KEY_U, false),
            key_event(KeyCode::KEY_B, false),
            key_event(KeyCode::KEY_L, false),
            key_event(KeyCode::KEY_E, false),
        ];
        for event in left_events {
            buffer.push(event);
        }
        buffer.handle_space();
        let current_events = [
            key_event(KeyCode::KEY_N, false),
            key_event(KeyCode::KEY_J, false),
            key_event(KeyCode::KEY_SEMICOLON, false),
            key_event(KeyCode::KEY_T, false),
        ];
        for event in current_events {
            buffer.push(event);
        }
        buffer.handle_space();

        let scope = effective_replace_words(&buffer, 2, CorrectionEngine::Smart, true);
        let (events, backspaces) = buffer.what_to_replay(scope).expect("last word tail");
        let decision = replay_layout_decision(&events);
        let current_original = map_original_events(&current_events);
        let current_target = map_events_to_layout(&current_events, true);

        assert_eq!(scope, 1);
        assert_eq!(map_original_events(&events), format!("{current_original} "));
        assert_eq!(
            map_events_to_layout(&events, decision.target_is_ru),
            format!("{current_target} ")
        );
        assert_eq!(backspaces, current_original.chars().count() as u32 + 1);
    }

    #[test]
    fn scoped_tail_flips_cyrillic_hyphen_technical_token_to_ascii() {
        let mut buffer = WordBuffer::new();
        let left_events = [
            key_event(KeyCode::KEY_C, true),
            key_event(KeyCode::KEY_K, true),
            key_event(KeyCode::KEY_J, true),
            key_event(KeyCode::KEY_D, true),
            key_event(KeyCode::KEY_J, true),
        ];
        for event in left_events {
            buffer.push(event);
        }
        buffer.handle_space();
        let technical_events = [
            key_event(KeyCode::KEY_W, true),
            key_event(KeyCode::KEY_I, true),
            key_event(KeyCode::KEY_MINUS, true),
            key_event(KeyCode::KEY_F, true),
            key_event(KeyCode::KEY_I, true),
        ];
        for event in technical_events {
            buffer.push(event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let left = map_events_to_layout(&left_events, true);
        let typed_technical = map_events_to_layout(&technical_events, true);
        let target_technical = map_events_to_layout(&technical_events, false);

        assert_eq!(
            map_original_events(&events),
            format!("{left} {typed_technical}")
        );
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some(format!("{left} {target_technical}"))
        );
    }

    #[test]
    fn scoped_tail_keeps_unknown_previous_word_and_flips_cyrillic_hyphen_technical_token() {
        let mut buffer = WordBuffer::new();
        let left_events = [
            KeyEvent {
                keycode: KeyCode::KEY_SEMICOLON.code(),
                shift: true,
                layout_is_ru: true,
            },
            key_event(KeyCode::KEY_SEMICOLON, true),
            key_event(KeyCode::KEY_SEMICOLON, true),
            key_event(KeyCode::KEY_SEMICOLON, true),
        ];
        for event in left_events {
            buffer.push(event);
        }
        buffer.handle_space();
        let technical_events = [
            key_event(KeyCode::KEY_W, true),
            key_event(KeyCode::KEY_I, true),
            key_event(KeyCode::KEY_MINUS, true),
            key_event(KeyCode::KEY_F, true),
            key_event(KeyCode::KEY_I, true),
        ];
        for event in technical_events {
            buffer.push(event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let left = map_events_to_layout(&left_events, true);
        let typed_technical = map_events_to_layout(&technical_events, true);
        let target_technical = map_events_to_layout(&technical_events, false);

        assert_eq!(
            map_original_events(&events),
            format!("{left} {typed_technical}")
        );
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some(format!("{left} {target_technical}"))
        );
    }

    #[test]
    fn typing_assist_converts_wrong_layout_ascii_hyphen_token() {
        let technical_events = [
            key_event(KeyCode::KEY_W, true),
            key_event(KeyCode::KEY_I, true),
            key_event(KeyCode::KEY_MINUS, true),
            key_event(KeyCode::KEY_F, true),
            key_event(KeyCode::KEY_I, true),
        ];
        let typed_technical = map_events_to_layout(&technical_events, true);
        let target_technical = map_events_to_layout(&technical_events, false);
        assert_eq!(
            apply_typing_assist_exact(&format!("{typed_technical} ")),
            Some(format!("{target_technical} "))
        );
    }

    #[test]
    fn plain_cyrillic_scope_word_does_not_become_ascii_technical_noise() {
        let events = [
            key_event(KeyCode::KEY_A, true),
            key_event(KeyCode::KEY_Q, true),
            key_event(KeyCode::KEY_DOT, true),
            key_event(KeyCode::KEY_Z, true),
        ];
        let original = map_events_to_layout(&events, true);
        let converted = map_events_to_layout(&events, false);

        assert!(original.chars().all(is_cyrillic_letter));
        assert!(is_ascii_technical_token(&converted));
        assert!(should_keep_plain_cyrillic_before_ascii_technical(
            &original, &converted
        ));
        assert_eq!(decide_completed_scope_word(&events), original);
    }

    #[test]
    fn smart_scoped_tail_handles_large_mixed_language_pair_matrix() {
        let english_left = [
            "good", "test", "word", "live", "double", "text", "mode", "file", "code", "data",
        ];
        let russian_left = [
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
        ];
        let russian_targets = [
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
        ];
        let english_targets = [
            "good", "test", "word", "live", "double", "text", "mode", "file", "code", "data",
        ];

        let mut cases = 0;
        for left in english_left {
            for target in russian_targets {
                let typed = lay::dict::convert(target, lay::dict::Direction::Ru2Us);
                assert_smart_pair(left, false, &typed, false, &format!("{left} {target}"));
                cases += 1;
            }
        }

        for left in russian_left {
            for target in english_targets {
                let typed = lay::dict::convert(target, lay::dict::Direction::Us2Ru);
                assert_smart_pair(left, true, &typed, true, &format!("{left} {target}"));
                cases += 1;
            }
        }

        assert!(cases >= 100, "expected at least 100 mixed pair cases");
    }

    #[test]
    fn scoped_tail_flips_current_visual_latin_word_with_cyrillic_c_homoglyph() {
        let mut buffer = WordBuffer::new();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_H, false),
                (KeyCode::KEY_E, false),
                (KeyCode::KEY_C, false),
                (KeyCode::KEY_K, false),
            ],
            false,
        );
        buffer.handle_space();
        buffer.push(key_event(KeyCode::KEY_C, true));
        buffer.push(key_event(KeyCode::KEY_H, false));
        buffer.push(key_event(KeyCode::KEY_E, false));
        buffer.push(key_event(KeyCode::KEY_C, false));
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(map_original_events(&events), "check сhec");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("check срус".to_string())
        );
    }

    #[test]
    fn scoped_tail_removes_duplicate_layout_prefix_from_completed_ascii_technical_token() {
        let mut buffer = WordBuffer::new();
        let mut completed_events = vec![key_event(KeyCode::KEY_W, true)];
        completed_events.extend(key_events(&ascii_hyphen_token_keycodes(), false));
        for event in &completed_events {
            buffer.push(*event);
        }
        buffer.handle_space();
        let current_events = key_events(&[KeyCode::KEY_G, KeyCode::KEY_H, KeyCode::KEY_J], false);
        for event in &current_events {
            buffer.push(*event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let completed_original = map_original_events(&completed_events);
        let current_original = map_original_events(&current_events);
        let completed_repaired =
            correct_duplicate_layout_prefix_on_ascii_token(&completed_original)
                .expect("duplicate prefix repair");
        let current_target = map_events_to_layout(&current_events, true);

        assert_eq!(
            map_original_events(&events),
            format!("{completed_original} {current_original}")
        );
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some(format!("{completed_repaired} {current_target}"))
        );
    }

    #[test]
    fn scoped_tail_keeps_ascii_hyphen_word_and_flips_current_short_tail() {
        let mut buffer = WordBuffer::new();
        let completed_events = key_events(&ascii_hyphen_token_keycodes(), false);
        for event in &completed_events {
            buffer.push(*event);
        }
        buffer.handle_space();
        let current_events = key_events(&[KeyCode::KEY_Y, KeyCode::KEY_E], false);
        for event in &current_events {
            buffer.push(*event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let completed_original = map_original_events(&completed_events);
        let current_original = map_original_events(&current_events);
        let current_target = map_events_to_layout(&current_events, true);

        assert_eq!(
            map_original_events(&events),
            format!("{completed_original} {current_original}")
        );
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some(format!("{completed_original} {current_target}"))
        );
        assert_eq!(
            plan_text_replacement(
                &format!("{completed_original} {current_original}"),
                &format!("{completed_original} {current_target}")
            ),
            Some(TextReplacement {
                move_left: 0,
                backspaces: current_original.chars().count() as u32,
                insert: current_target,
                move_right: 0,
            })
        );
    }

    #[test]
    fn trailing_space_replay_flips_last_short_word_after_ascii_hyphen_word() {
        let mut buffer = WordBuffer::new();
        let completed_events = key_events(&ascii_hyphen_token_keycodes(), false);
        for event in &completed_events {
            buffer.push(*event);
        }
        buffer.handle_space();
        let current_events = key_events(&[KeyCode::KEY_Y, KeyCode::KEY_E], false);
        for event in &current_events {
            buffer.push(*event);
        }
        buffer.handle_space();

        let scope = effective_replace_words(&buffer, 2, CorrectionEngine::Smart, true);
        let (events, backspaces) = buffer.what_to_replay(scope).expect("last word tail");
        let decision = replay_layout_decision(&events);
        let current_original = map_original_events(&current_events);
        let current_target = map_events_to_layout(&current_events, true);

        assert_eq!(scope, 1);
        assert_eq!(map_original_events(&events), format!("{current_original} "));
        assert_eq!(
            map_events_to_layout(&events, decision.target_is_ru),
            format!("{current_target} ")
        );
        assert_eq!(backspaces, current_original.chars().count() as u32 + 1);
    }

    #[test]
    fn scoped_tail_collapses_cyrillic_prefix_before_ascii_hyphen_tail() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_C,
                KeyCode::KEY_K,
                KeyCode::KEY_J,
                KeyCode::KEY_D,
                KeyCode::KEY_J,
            ],
            true,
        );
        buffer.handle_space();
        let mut current_events = vec![key_event(KeyCode::KEY_G, true)];
        current_events.extend(key_events(
            &[
                KeyCode::KEY_G,
                KeyCode::KEY_F,
                KeyCode::KEY_H,
                KeyCode::KEY_F,
                KeyCode::KEY_MINUS,
                KeyCode::KEY_G,
                KeyCode::KEY_F,
                KeyCode::KEY_H,
                KeyCode::KEY_F,
            ],
            false,
        ));
        for event in &current_events {
            buffer.push(*event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let left = map_events_to_layout(
            &[
                key_event(KeyCode::KEY_C, true),
                key_event(KeyCode::KEY_K, true),
                key_event(KeyCode::KEY_J, true),
                key_event(KeyCode::KEY_D, true),
                key_event(KeyCode::KEY_J, true),
            ],
            true,
        );
        let current_original = map_original_events(&current_events);
        let current_target = repair_cyrillic_prefix_before_ascii_tail(&current_events)
            .expect("prefix collapse repair");

        assert_eq!(
            map_original_events(&events),
            format!("{left} {current_original}")
        );
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some(format!("{left} {current_target}"))
        );
    }

    #[test]
    fn scoped_tail_repairs_mixed_cyrillic_prefix_ascii_hyphen_word_and_keeps_undo() {
        let mut buffer = WordBuffer::new();
        push_text_as_layout(&mut buffer, "Иракскую", true);
        buffer.handle_space();
        buffer.push(text_key_event('к', true));
        for ch in "jrf-rjke".chars() {
            buffer.push(text_key_event(ch, false));
        }

        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let original = map_original_events(&events);
        let replacement = decide_scoped_tail_correction(&events).expect("smart replacement");
        let plan = plan_text_replacement(&original, &replacement).expect("minimal plan");

        assert_eq!(original, "Иракскую кjrf-rjke");
        assert_eq!(replacement, "Иракскую кока-колу");
        assert_eq!(
            plan,
            TextReplacement {
                move_left: 0,
                backspaces: 8,
                insert: "ока-колу".to_string(),
                move_right: 0,
            }
        );
        assert!(buffer.remember_replacement_last_word_for_replay(&events, &plan, &replacement));

        let (undo_events, undo_backspaces) = buffer.what_to_replay(2).expect("undo tail");
        let undo_decision = replay_layout_decision(&undo_events);
        assert_eq!(map_original_events(&undo_events), "кока-колу");
        assert_eq!(undo_backspaces, 9);
        assert!(!undo_decision.target_is_ru);
        assert_eq!(map_events_to_layout(&undo_events, false), "rjrf-rjke");
        assert!(buffer.replay_toggle_ready());
    }

    #[test]
    fn replacement_last_word_memory_ignores_middle_insert_plan() {
        let mut buffer = WordBuffer::new();
        push_text_as_layout(&mut buffer, "AmoCRM", false);
        buffer.handle_space();
        push_text_as_layout(&mut buffer, "Z", false);
        buffer.handle_space();
        push_text_as_layout(&mut buffer, "тут", true);
        buffer.handle_space();
        push_text_as_layout(&mut buffer, "задача", true);

        let (events, _) = buffer.what_to_replay(4).expect("four-word tail");
        let plan = plan_text_replacement("AmoCRM Z тут задача", "AmoCRM Я тут задача")
            .expect("middle replacement plan");

        assert_eq!(plan.move_right, 11);
        assert!(!buffer.remember_replacement_last_word_for_replay(
            &events,
            &plan,
            "AmoCRM Я тут задача"
        ));
    }

    #[test]
    fn scoped_tail_does_not_turn_valid_ascii_hyphen_tail_into_bad_russian() {
        let mut buffer = WordBuffer::new();
        push_keys(&mut buffer, &[KeyCode::KEY_D], true);
        buffer.handle_space();
        let mut current_events = vec![key_event(KeyCode::KEY_W, true)];
        current_events.extend([
            KeyEvent {
                keycode: KeyCode::KEY_W.code(),
                shift: true,
                layout_is_ru: false,
            },
            key_event(KeyCode::KEY_I, false),
            key_event(KeyCode::KEY_MINUS, false),
            key_event(KeyCode::KEY_F, false),
            key_event(KeyCode::KEY_I, false),
        ]);
        for event in &current_events {
            buffer.push(*event);
        }
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");
        let left = map_events_to_layout(&[key_event(KeyCode::KEY_D, true)], true);
        let current_original = map_original_events(&current_events);
        let current_wrong_layout = map_events_to_layout(&current_events, true);

        assert_eq!(
            map_original_events(&events),
            format!("{left} {current_original}")
        );
        assert_ne!(
            decide_scoped_tail_correction(&events),
            Some(format!("{left} {current_wrong_layout}"))
        );
    }

    #[test]
    fn scoped_tail_converts_confident_bad_previous_word() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_G,
                KeyCode::KEY_H,
                KeyCode::KEY_B,
                KeyCode::KEY_D,
                KeyCode::KEY_T,
                KeyCode::KEY_N,
            ],
            false,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[KeyCode::KEY_V, KeyCode::KEY_B, KeyCode::KEY_H],
            false,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("привет мир".to_string())
        );
    }

    #[test]
    fn scoped_tail_keeps_unknown_previous_word() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_F,
                KeyCode::KEY_O,
                KeyCode::KEY_O,
                KeyCode::KEY_B,
                KeyCode::KEY_A,
                KeyCode::KEY_R,
            ],
            false,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[KeyCode::KEY_G, KeyCode::KEY_H, KeyCode::KEY_J],
            false,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("foobar про".to_string())
        );
    }

    #[test]
    fn scoped_tail_generalizes_to_more_than_two_words() {
        let mut buffer = WordBuffer::new();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_G,
                KeyCode::KEY_H,
                KeyCode::KEY_J,
                KeyCode::KEY_D,
                KeyCode::KEY_T,
                KeyCode::KEY_H,
                KeyCode::KEY_R,
                KeyCode::KEY_F,
            ],
            true,
        );
        buffer.handle_space();
        push_keys(&mut buffer, &[KeyCode::KEY_D], true);
        buffer.handle_space();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_D, true),
                (KeyCode::KEY_O, false),
                (KeyCode::KEY_U, false),
                (KeyCode::KEY_B, false),
                (KeyCode::KEY_L, false),
                (KeyCode::KEY_E, false),
            ],
            true,
        );
        let (events, _) = buffer.what_to_replay(3).expect("three-word tail");

        assert_eq!(map_original_events(&events), "проверка в Вщгиду");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("проверка в Double".to_string())
        );
    }

    #[test]
    fn scoped_tail_keeps_live_and_flips_russian_current_tail() {
        let mut buffer = WordBuffer::new();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_L, true),
                (KeyCode::KEY_I, false),
                (KeyCode::KEY_V, false),
                (KeyCode::KEY_E, false),
            ],
            false,
        );
        buffer.handle_space();
        push_keys(
            &mut buffer,
            &[
                KeyCode::KEY_L,
                KeyCode::KEY_B,
                KeyCode::KEY_C,
                KeyCode::KEY_N,
                KeyCode::KEY_H,
                KeyCode::KEY_B,
            ],
            false,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(map_original_events(&events), "Live lbcnhb");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("Live дистри".to_string())
        );
    }

    #[test]
    fn scoped_tail_normalizes_mixed_current_word_to_last_layout() {
        let mut buffer = WordBuffer::new();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_L, true),
                (KeyCode::KEY_I, false),
                (KeyCode::KEY_V, false),
                (KeyCode::KEY_E, false),
            ],
            false,
        );
        buffer.handle_space();
        push_keys(&mut buffer, &[KeyCode::KEY_L], false);
        push_keys(
            &mut buffer,
            &[KeyCode::KEY_L, KeyCode::KEY_B, KeyCode::KEY_C],
            true,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(map_original_events(&events), "Live lдис");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("Live дис".to_string())
        );
    }

    #[test]
    fn scoped_tail_repairs_mixed_previous_ru_word_and_flips_current_tail() {
        let mut buffer = WordBuffer::new();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_G, true),
                (KeyCode::KEY_H, true),
                (KeyCode::KEY_J, true),
                (KeyCode::KEY_D, true),
            ],
            true,
        );
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_T, true),
                (KeyCode::KEY_H, true),
                (KeyCode::KEY_M, true),
            ],
            false,
        );
        buffer.handle_space();
        push_key_events(
            &mut buffer,
            &[
                (KeyCode::KEY_W, true),
                (KeyCode::KEY_O, true),
                (KeyCode::KEY_R, true),
                (KeyCode::KEY_D, true),
            ],
            true,
        );
        let (events, _) = buffer.what_to_replay(2).expect("two-word tail");

        assert_eq!(map_original_events(&events), "ПРОВTHM ЦЩКВ");
        assert_eq!(
            decide_scoped_tail_correction(&events),
            Some("ПРОВЕРЬ WORD".to_string())
        );
    }

    #[test]
    fn smart_decision_replays_single_valid_word_as_manual_toggle() {
        assert_eq!(
            decide_correction("DOUBLE", "ВЩГИДУ", CorrectionEngine::Smart),
            Correction::ReplayAll
        );
    }

    #[test]
    fn smart_decision_replays_single_cyrillic_acronym_as_manual_toggle() {
        let events = [
            KeyEvent {
                keycode: KeyCode::KEY_L.code(),
                shift: true,
                layout_is_ru: true,
            },
            KeyEvent {
                keycode: KeyCode::KEY_L.code(),
                shift: true,
                layout_is_ru: true,
            },
            KeyEvent {
                keycode: KeyCode::KEY_M.code(),
                shift: true,
                layout_is_ru: true,
            },
        ];
        let decision = replay_layout_decision(&events);
        let original = map_original_events(&events);
        let target = map_events_to_layout(&events, decision.target_is_ru);

        assert_eq!(original, "ДДЬ");
        assert_eq!(target, "LLM");
        assert!(!decision.target_is_ru);
        assert_eq!(
            decide_correction(&original, &target, CorrectionEngine::Smart),
            Correction::ReplayAll
        );
    }

    #[test]
    fn smart_decision_replays_two_valid_words_as_manual_toggle() {
        assert_eq!(
            decide_correction("выводим два", "dsdjlbv ldf", CorrectionEngine::Smart),
            Correction::ReplayAll
        );
    }

    #[test]
    fn smart_decision_replays_valid_russian_preposition_phrase_as_manual_toggle() {
        assert_eq!(
            decide_correction("в доме", "d ljvt", CorrectionEngine::Smart),
            Correction::ReplayAll
        );
    }

    #[test]
    fn smart_decision_converts_mixed_layout_neighbor_only() {
        assert_eq!(
            decide_correction("рка ghj", "hrf про", CorrectionEngine::Smart),
            Correction::InsertText("рка про".to_string())
        );
        assert_eq!(
            decide_correction("проверка ghj", "ghjdthrf про", CorrectionEngine::Smart),
            Correction::InsertText("проверка про".to_string())
        );
    }

    #[test]
    fn smart_decision_replays_protected_ascii_span_as_manual_toggle() {
        assert_eq!(
            decide_correction("AmoCRM Я", "ФьщСКЬ Z", CorrectionEngine::Smart),
            Correction::ReplayAll
        );
    }

    #[test]
    fn smart_decision_repairs_brand_plus_letter_inside_larger_tail() {
        assert_eq!(
            decide_correction(
                "AmoCRM Z тут задача",
                "ФьщСКЬ Я nen pflfxf",
                CorrectionEngine::Smart
            ),
            Correction::InsertText("AmoCRM Я тут задача".to_string())
        );
    }

    #[test]
    fn replacement_plan_keeps_good_suffix_in_place() {
        assert_eq!(
            plan_text_replacement("NEN DOUBLE", "ТУТ DOUBLE"),
            Some(TextReplacement {
                move_left: 7,
                backspaces: 3,
                insert: "ТУТ".to_string(),
                move_right: 7,
            })
        );
    }

    #[test]
    fn replacement_plan_keeps_good_prefix_in_place() {
        assert_eq!(
            plan_text_replacement("Главное Вщгиду", "Главное Double"),
            Some(TextReplacement {
                move_left: 0,
                backspaces: 6,
                insert: "Double".to_string(),
                move_right: 0,
            })
        );
    }

    #[test]
    fn replacement_plan_replaces_single_bad_middle_token() {
        assert_eq!(
            plan_text_replacement("AmoCRM Z тут задача", "AmoCRM Я тут задача"),
            Some(TextReplacement {
                move_left: 11,
                backspaces: 1,
                insert: "Я".to_string(),
                move_right: 11,
            })
        );
    }

    #[test]
    fn replacement_plan_deletes_duplicate_prefix_before_kept_suffix() {
        assert_eq!(
            plan_text_replacement("на ппредмет", "на предмет"),
            Some(TextReplacement {
                move_left: 6,
                backspaces: 1,
                insert: String::new(),
                move_right: 6,
            })
        );
    }

    #[test]
    fn opposite_events_flip_each_key_own_layout_for_smart_mixed_tail() {
        let events = [
            key_event(KeyCode::KEY_H, true),
            key_event(KeyCode::KEY_R, true),
            key_event(KeyCode::KEY_F, true),
            key_event(KeyCode::KEY_SPACE, false),
            key_event(KeyCode::KEY_G, false),
            key_event(KeyCode::KEY_H, false),
            key_event(KeyCode::KEY_J, false),
        ];

        assert_eq!(map_original_events(&events), "рка ghj");
        assert_eq!(map_opposite_events(&events), "hrf про");
    }

    #[test]
    fn smart_insert_layout_follows_result_text_tail() {
        assert!(preferred_layout_for_text("рка про", false));
        assert!(!preferred_layout_for_text("Главное Double", true));
        assert!(preferred_layout_for_text("AmoCRM Я тут задача", false));
    }

    #[test]
    fn target_layout_matches_cache_contract() {
        assert_eq!(target_layout(true), ("ru", "xkb:ru::rus"));
        assert_eq!(target_layout(false), ("us", "xkb:us::eng"));
    }

    #[test]
    fn typing_after_replay_clears_toggle_shortcut() {
        let mut buffer = WordBuffer::new();
        buffer.push(KeyEvent {
            keycode: KeyCode::KEY_D.code(),
            shift: false,
            layout_is_ru: false,
        });
        buffer.mark_replayed_layout(1, true);

        buffer.push(KeyEvent {
            keycode: KeyCode::KEY_H.code(),
            shift: false,
            layout_is_ru: true,
        });

        assert!(!buffer.replay_toggle_ready());
    }

    #[test]
    fn writes_learning_log_as_jsonl() {
        let tmp = std::env::temp_dir().join(format!(
            "lay-learn-log-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("corrections.jsonl");
        append_learning_log_to_path(&path, "layout-replay", "ghbdtn", "привет", 1, 1);
        let line = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["kind"], "layout-replay");
        assert_eq!(value["from"], "ghbdtn");
        assert_eq!(value["to"], "привет");
        assert!(value.get("lay_kind").is_none());

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn learning_feedback_records_user_fix_after_lay_correction() {
        let mut buffer = WordBuffer::new();
        buffer.remember_pending_learning_correction("typing-assist", "смотри ", "смотрин ", 1, 1);
        for _ in 0.."смотрин ".chars().count() {
            buffer.note_learning_backspace();
        }
        for key in [
            KeyCode::KEY_C,
            KeyCode::KEY_V,
            KeyCode::KEY_J,
            KeyCode::KEY_N,
            KeyCode::KEY_H,
            KeyCode::KEY_B,
        ] {
            buffer.note_learning_typed(key_event(key, true));
        }

        let correction = buffer
            .take_user_learning_correction(true)
            .expect("user correction should be captured");

        assert_eq!(
            correction,
            UserLearningCorrection {
                lay_kind: "typing-assist".to_string(),
                lay_from: "смотри ".to_string(),
                lay_to: "смотрин ".to_string(),
                from: "смотрин ".to_string(),
                to: "смотри ".to_string(),
                replace_words: 1,
                words: 1,
            }
        );
    }

    #[test]
    fn learning_feedback_ignores_lay_output_without_user_edit() {
        let mut buffer = WordBuffer::new();
        buffer.remember_pending_learning_correction("typing-assist", "смотри ", "смотрин ", 1, 1);
        buffer.note_learning_typed(key_event(KeyCode::KEY_G, true));

        assert!(buffer.take_user_learning_correction(true).is_none());
    }

    #[test]
    fn learning_feedback_does_not_attach_space_to_non_space_correction() {
        let mut buffer = WordBuffer::new();
        buffer.remember_pending_learning_correction("smart-text", "abc", "abd", 1, 1);
        buffer.note_learning_backspace();
        buffer.note_learning_typed(key_event(KeyCode::KEY_C, false));

        let correction = buffer
            .take_user_learning_correction(true)
            .expect("user correction should be captured");

        assert_eq!(correction.from, "d");
        assert_eq!(correction.to, "c");
    }

    #[test]
    fn writes_user_correction_learning_log_with_lay_context() {
        let tmp = std::env::temp_dir().join(format!(
            "lay-user-learn-log-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("corrections.jsonl");
        append_user_correction_learning_log_to_path(
            &path,
            &UserLearningCorrection {
                lay_kind: "typing-assist".to_string(),
                lay_from: "смотри ".to_string(),
                lay_to: "смотрин ".to_string(),
                from: "смотрин ".to_string(),
                to: "смотри ".to_string(),
                replace_words: 1,
                words: 1,
            },
        );

        let line = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["kind"], "user-correction");
        assert_eq!(value["from"], "смотрин ");
        assert_eq!(value["to"], "смотри ");
        assert_eq!(value["lay_kind"], "typing-assist");
        assert_eq!(value["lay_from"], "смотри ");
        assert_eq!(value["lay_to"], "смотрин ");

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn repeated_user_correction_promotes_exact_rule() {
        let tmp = std::env::temp_dir().join(format!(
            "lay-learn-promote-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let candidates = tmp.join("learning_candidates.json");
        let replacements = tmp.join("replacements.json");
        let correction = UserLearningCorrection {
            lay_kind: "typing-assist".to_string(),
            lay_from: "смотри ".to_string(),
            lay_to: "смотриии ".to_string(),
            from: "смотриии ".to_string(),
            to: "смотри ".to_string(),
            replace_words: 1,
            words: 1,
        };

        assert_eq!(
            promote_user_correction_if_repeated(&candidates, &replacements, &correction),
            LearningPromotion::Recorded {
                from: "смотриии".to_string(),
                to: "смотри".to_string(),
                count: 1,
            }
        );
        assert!(!replacements.exists());

        assert_eq!(
            promote_user_correction_if_repeated(&candidates, &replacements, &correction),
            LearningPromotion::Promoted {
                from: "смотриии".to_string(),
                to: "смотри".to_string(),
            }
        );

        let rules: BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&replacements).unwrap()).unwrap();
        assert_eq!(rules.get("смотриии"), Some(&"смотри".to_string()));
        assert_eq!(
            promoted_replacement_for_token("Смотриии"),
            Some("Смотри".to_string())
        );

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn learning_promotion_skips_unsafe_short_edits() {
        let tmp = std::env::temp_dir().join(format!(
            "lay-learn-skip-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let correction = UserLearningCorrection {
            lay_kind: "auto-replace".to_string(),
            lay_from: "b ".to_string(),
            lay_to: "в ".to_string(),
            from: "в ".to_string(),
            to: "и ".to_string(),
            replace_words: 1,
            words: 1,
        };

        assert_eq!(
            promote_user_correction_if_repeated(
                &tmp.join("learning_candidates.json"),
                &tmp.join("replacements.json"),
                &correction,
            ),
            LearningPromotion::Skipped
        );
        assert!(!tmp.join("replacements.json").exists());

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn parses_gdbus_bool_tuple() {
        assert_eq!(parse_gdbus_bool("(true,)"), Some(true));
        assert_eq!(parse_gdbus_bool("(false,)"), Some(false));
        assert_eq!(parse_gdbus_bool("true"), None);
    }

    #[test]
    fn keeps_only_last_jsonl_lines() {
        let compacted = keep_last_jsonl_lines("a\nb\nc\nd\n", 2);
        assert_eq!(compacted, "c\nd\n");
    }

    #[test]
    fn applies_builtin_auto_replace_with_trailing_space() {
        assert_eq!(
            apply_auto_replace("gjlk.xbcm ", "подлючись "),
            Some("подключись ".to_string())
        );
        assert_eq!(apply_auto_replace("Tcnm ", "Есть "), None);
    }

    #[test]
    fn typing_assist_uses_exact_rules_only() {
        assert_eq!(
            apply_typing_assist_exact("подлючись "),
            Some("подключись ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("Надйи "),
            Some("Найди ".to_string())
        );
        assert_eq!(apply_typing_assist_exact("нормально "), None);
        assert_eq!(apply_typing_assist_exact("Есть "), None);
    }

    #[test]
    fn typing_assist_auto_switch_converts_confident_wrong_layout_words() {
        assert_eq!(
            apply_typing_assist("njkmrj ", true),
            Some("только ".to_string())
        );
        assert_eq!(apply_typing_assist("yt ", true), Some("не ".to_string()));
        assert_eq!(
            apply_typing_assist("hf,jnftn ", true),
            Some("работает ".to_string())
        );
        assert_eq!(
            apply_typing_assist("njkmrj ", false),
            None,
            "auto layout word repair must stay behind the tray checkbox"
        );
    }

    #[test]
    fn typing_assist_auto_switch_keeps_english_and_protected_ascii() {
        assert_eq!(apply_typing_assist("hello ", true), None);
        assert_eq!(apply_typing_assist("test ", true), None);
        assert_eq!(apply_typing_assist("good ", true), None);
        assert_eq!(apply_typing_assist("API ", true), None);
        assert_eq!(apply_typing_assist("AmoCRM ", true), None);
        assert_eq!(apply_typing_assist("wi-fi ", true), None);
    }

    #[test]
    fn typing_assist_fixes_adjacent_transposition() {
        assert_eq!(
            apply_typing_assist_exact("рабоатет "),
            Some("работает ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("Проверак "),
            Some("Проверка ".to_string())
        );
    }

    #[test]
    fn typing_assist_fixes_small_glued_words() {
        assert_eq!(
            apply_typing_assist_exact("нуда "),
            Some("ну да ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("вчем "),
            Some("в чем ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("Вчем, "),
            Some("В чем, ".to_string())
        );
    }

    #[test]
    fn typing_assist_fixes_common_missing_letter_typos() {
        assert_eq!(
            apply_typing_assist_exact("првильно "),
            Some("правильно ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("Првильно "),
            Some("Правильно ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("можн "),
            Some("можно ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("Можн "),
            Some("Можно ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("дльше "),
            Some("дальше ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("дальг "),
            Some("дальше ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("плозо "),
            Some("плохо ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("фактческим "),
            Some("фактическим ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("иблиотеку "),
            Some("библиотеку ".to_string())
        );
    }

    #[test]
    fn typing_assist_normalizes_accidental_inner_uppercase() {
        assert_eq!(
            apply_typing_assist_exact("МОжно "),
            Some("Можно ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("моЖно "),
            Some("можно ".to_string())
        );
        assert_eq!(apply_typing_assist_exact("МОЖНО "), None);
    }

    #[test]
    fn typing_assist_single_letter_typos_only_use_neighbor_keys() {
        assert!(are_ru_keyboard_neighbors('з', 'х'));
        assert!(!are_ru_keyboard_neighbors('о', 'ь'));
        assert_eq!(apply_typing_assist_exact("покрыто "), None);
    }

    #[test]
    fn typing_assist_merges_accidental_space_inside_word() {
        assert_eq!(
            apply_typing_assist_exact("я вно "),
            Some("явно ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("Я вно, "),
            Some("Явно, ".to_string())
        );
        assert_eq!(apply_typing_assist_exact("я тут "), None);
        assert_eq!(apply_typing_assist_exact("чтобы точно "), None);
        assert_eq!(apply_typing_assist_exact("хо хо "), None);
    }

    #[test]
    fn typing_assist_fixes_hard_sign_typos() {
        assert_eq!(
            apply_typing_assist_exact("Обьясни "),
            Some("Объясни ".to_string())
        );
    }

    #[test]
    fn typing_assist_moves_letter_from_next_word_back() {
        assert_eq!(
            apply_typing_assist_exact("расчет ыприблизительные "),
            Some("расчеты приблизительные ".to_string())
        );
        assert_eq!(
            apply_typing_assist_to_text_tail("все расчет ыприблизительные "),
            Some("все расчеты приблизительные ".to_string())
        );
    }

    #[test]
    fn typing_assist_removes_duplicate_layout_prefix_from_ascii_technical_token() {
        let prefix_lower = map_events_to_layout(&[key_event(KeyCode::KEY_W, true)], true);
        let prefix_upper = map_events_to_layout(
            &[KeyEvent {
                keycode: KeyCode::KEY_W.code(),
                shift: true,
                layout_is_ru: true,
            }],
            true,
        );
        let technical_lower =
            map_events_to_layout(&key_events(&ascii_hyphen_token_keycodes(), false), false);
        let technical_upper = map_events_to_layout(
            &[
                KeyEvent {
                    keycode: KeyCode::KEY_W.code(),
                    shift: true,
                    layout_is_ru: false,
                },
                key_event(KeyCode::KEY_I, false),
                key_event(KeyCode::KEY_MINUS, false),
                KeyEvent {
                    keycode: KeyCode::KEY_F.code(),
                    shift: true,
                    layout_is_ru: false,
                },
                key_event(KeyCode::KEY_I, false),
            ],
            false,
        );
        let no_separator = map_events_to_layout(
            &key_events(
                &[
                    KeyCode::KEY_W,
                    KeyCode::KEY_I,
                    KeyCode::KEY_F,
                    KeyCode::KEY_I,
                ],
                false,
            ),
            false,
        );

        assert_eq!(
            apply_typing_assist_exact(&format!("{prefix_lower}{technical_lower} ")),
            Some(format!("{technical_lower} "))
        );
        assert_eq!(
            apply_typing_assist_exact(&format!("{prefix_upper}{technical_upper}, ")),
            Some(format!("{technical_upper}, "))
        );
        assert_eq!(
            apply_typing_assist_exact(&format!("{prefix_lower}{no_separator} ")),
            None
        );
    }

    #[test]
    fn typing_assist_does_not_move_normal_word_prefixes() {
        assert_eq!(apply_typing_assist_exact("схеме таможенник "), None);
        assert_eq!(apply_typing_assist_exact("схема таможженик "), None);
    }

    #[test]
    fn typing_assist_fixes_extra_repeated_letter() {
        assert_eq!(
            apply_typing_assist_exact("исправленно "),
            Some("исправлено ".to_string())
        );
        assert_eq!(
            apply_typing_assist_exact("исправленнно "),
            Some("исправлено ".to_string())
        );
    }

    #[test]
    fn typing_assist_keeps_valid_russian_words() {
        assert_eq!(apply_typing_assist_exact("проверка "), None);
        assert_eq!(apply_typing_assist_exact("работает "), None);
        assert_eq!(apply_typing_assist_exact("привет "), None);
        assert_eq!(apply_typing_assist_exact("можем "), None);
        assert_eq!(apply_typing_assist_exact("можешь "), None);
        assert_eq!(apply_typing_assist_exact("может "), None);
        assert_eq!(apply_typing_assist_exact("ладно "), None);
        assert_eq!(apply_typing_assist_exact("можно "), None);
        assert_eq!(apply_typing_assist_exact("дальше "), None);
        assert_eq!(apply_typing_assist_exact("плохо "), None);
        assert_eq!(apply_typing_assist_exact("правильно "), None);
        assert_eq!(apply_typing_assist_exact("исправляет "), None);
        assert_eq!(apply_typing_assist_exact("удаляется "), None);
        assert_eq!(apply_typing_assist_exact("удателятеся "), None);
        assert_eq!(apply_typing_assist_exact("еще "), None);
        assert_eq!(apply_typing_assist_exact("елка "), None);
        assert_eq!(apply_typing_assist_exact("все "), None);
    }

    #[test]
    fn typing_assist_ignores_words_with_digits() {
        assert_eq!(apply_typing_assist_exact("товара7 "), None);
        assert_eq!(apply_typing_assist_exact("привемр7 "), None);
        assert_eq!(apply_typing_assist_exact("пример? привемр7 "), None);
    }

    #[test]
    fn typing_assist_regression_suite_100_cases() {
        let should_fix = [
            ("подлючись ", "подключись "),
            ("надйи ", "найди "),
            ("Надйи ", "Найди "),
            ("нуда ", "ну да "),
            ("Нуда ", "Ну да "),
            ("вчем ", "в чем "),
            ("Вчем, ", "В чем, "),
            ("можн ", "можно "),
            ("Можн ", "Можно "),
            ("МОжно ", "Можно "),
            ("моЖно ", "можно "),
            ("дльше ", "дальше "),
            ("Дльше ", "Дальше "),
            ("дальг ", "дальше "),
            ("првильно ", "правильно "),
            ("Првильно ", "Правильно "),
            ("рабоатет ", "работает "),
            ("Рабоатет ", "Работает "),
            ("Проверак ", "Проверка "),
            ("ошисбя ", "ошибся "),
            ("Ошисбя ", "Ошибся "),
            ("сиправить ", "исправить "),
            ("Сиправить ", "Исправить "),
            ("плозо ", "плохо "),
            ("Плозо ", "Плохо "),
            ("фактческим ", "фактическим "),
            ("иблиотеку ", "библиотеку "),
            ("Обьясни ", "Объясни "),
            ("исправленно ", "исправлено "),
            ("Исправленно ", "Исправлено "),
            ("исправленнно ", "исправлено "),
            ("я вно ", "явно "),
            ("Я вно, ", "Явно, "),
            (
                "все расчет ыприблизительные ",
                "все расчеты приблизительные ",
            ),
            ("тут я вно ", "тут явно "),
            ("Но я вно ", "Но явно "),
            ("подлючись. ", "подключись. "),
            ("надйи! ", "найди! "),
            ("можн? ", "можно? "),
            ("дльше, ", "дальше, "),
            ("првильно. ", "правильно. "),
            ("плозо! ", "плохо! "),
            ("ошисбя, ", "ошибся, "),
        ];

        for (input, expected) in should_fix {
            assert_eq!(
                apply_typing_assist_to_text_tail(input),
                Some(expected.to_string()),
                "input={input:?}"
            );
        }

        let should_keep = [
            "привет ",
            "проверка ",
            "работает ",
            "ошибка ",
            "ошибся ",
            "явно ",
            "ладно ",
            "можно ",
            "дальше ",
            "плохо ",
            "правильно ",
            "исправлено ",
            "исправляет ",
            "покрыто ",
            "покрыть ",
            "слово ",
            "текст ",
            "модель ",
            "режим ",
            "файл ",
            "проект ",
            "тест ",
            "код ",
            "корпус ",
            "кеш ",
            "лог ",
            "демон ",
            "помощник ",
            "клавиатура ",
            "раскладка ",
            "буфер ",
            "пробел ",
            "сейчас ",
            "потом ",
            "очень ",
            "нужно ",
            "хорошо ",
            "плохо ",
            "сделал ",
            "проверил ",
            "пишу ",
            "печатаю ",
            "быстро ",
            "медленно ",
            "нормально ",
            "отлично ",
            "давай ",
            "нет ",
            "вот ",
            "это ",
            "как ",
            "что ",
            "если ",
            "тогда ",
            "тут ",
            "там ",
            "уже ",
            "ещё ",
            "не ",
            "ни ",
            "хо хо ",
            "ха ха ",
            "CPU ",
            "LLM ",
            "API ",
            "МГУ ",
            "README ",
            "GitHub ",
            "WeChat ",
            "hello ",
            "world ",
            "cargo ",
            "Rust ",
            "GNOME ",
            "Wayland ",
            "Ollama ",
            "Qwen ",
            "BitNet ",
            "smollm ",
            "conecargo.ru ",
            "test@example.com ",
            "https://example.com ",
            "123 ",
            "7 ",
            "b/ ",
            "и. ",
            "в магазин ",
            "в вот ",
            "машина ",
            "магазин ",
            "схеме таможенник ",
            "схема таможженик ",
            "пошли ",
            "пошли в ",
            "ни фига ",
            "не фига ",
            "как говорится ",
            "ну что же ",
        ];

        for input in should_keep {
            assert_eq!(
                apply_typing_assist_to_text_tail(input),
                None,
                "input={input:?}"
            );
        }

        let total = should_fix.len() + should_keep.len();
        assert!(
            total >= 100,
            "regression suite should keep at least 100 cases, got {total}"
        );
    }

    #[test]
    fn auto_replace_regression_suite() {
        let cases = [
            ("перейти b", "gthtqnb b", "перейти в"),
            ("b ghjcnj", "и просто", "в просто"),
            ("слово b ", "слово и ", "слово в "),
            ("b vfufpby ", "и магазин ", "в магазин "),
            ("b djn", "и вот", "в вот"),
        ];

        for (original, target, expected) in cases {
            assert_eq!(
                apply_auto_replace(original, target),
                Some(expected.to_string()),
                "original={original:?} target={target:?}"
            );
        }
    }

    #[test]
    fn replaces_visual_b_inside_russian_context() {
        assert_eq!(
            apply_auto_replace("перейти b", "gthtqnb b"),
            Some("перейти в".to_string())
        );
        assert_eq!(
            apply_auto_replace("b ghjcnj", "и просто"),
            Some("в просто".to_string())
        );
        assert_eq!(
            apply_auto_replace("слово b ", "слово и "),
            Some("слово в ".to_string())
        );
    }
}
