//! X11 backend: переключение раскладки и эмуляция ввода без DBus/GNOME.
//!
//! - `XkbLockGroup` для синхронного переключения раскладки.
//! - `XkbGetState` для чтения активной группы.
//! - `XTest fake_input` для fallback ввода текста (когда uinput-replay не подходит).
//!
//! Раскладки задаются в системе через `setxkbmap -layout us,ru`. Daemon работает
//! в терминах номеров групп (0 = первая, 1 = вторая, ...). Маппинг
//! «номер группы → имя» строится один раз при старте через `setxkbmap -query`
//! или через XKB символы. На практике для нашего сценария важно:
//!   - target group 0 — "us" (US-английская),
//!   - target group 1 — "ru" (русская).
//!
//! Если у пользователя порядок другой, его можно задать в config через поле
//! `layout_groups` (см. lay-daemon).

use std::sync::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::xkb::ConnectionExt as _;
use x11rb::protocol::xproto::ModMask;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// Спец-значение «использовать текущее ядро устройства» в XKB.
const XKB_USE_CORE_KBD: u16 = 0x0100;

struct Backend {
    conn: RustConnection,
    root: u32,
}

impl Backend {
    fn connect() -> Result<Self, String> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| format!("X11 connect: {e}"))?;
        let root = conn.setup().roots[screen_num].root;

        // Активируем XKB-расширение (обязательно — без UseExtension XKB-запросы вернут ошибку).
        conn.xkb_use_extension(1, 0)
            .map_err(|e| format!("xkb_use_extension send: {e}"))?
            .reply()
            .map_err(|e| format!("xkb_use_extension reply: {e}"))?;

        Ok(Self { conn, root })
    }

    fn current_group(&self) -> Result<u8, String> {
        let state = self
            .conn
            .xkb_get_state(XKB_USE_CORE_KBD)
            .map_err(|e| format!("xkb_get_state send: {e}"))?
            .reply()
            .map_err(|e| format!("xkb_get_state reply: {e}"))?;
        Ok(state.group.into())
    }

    fn lock_group(&self, group: u8) -> Result<(), String> {
        // LatchLockState: меняет locked_group атомарно.
        // affect_mod_locks=0, mod_locks=0 (модификаторы не трогаем),
        // lock_group=true, group_lock=<target>.
        let no_mods = ModMask::from(0u16);
        self.conn
            .xkb_latch_lock_state(
                XKB_USE_CORE_KBD,
                no_mods,          // affect_mod_locks
                no_mods,          // mod_locks
                true,             // lock_group
                group.into(),     // group_lock
                no_mods,          // affect_mod_latches
                false,            // latch_group
                0,                // group_latch
            )
            .map_err(|e| format!("xkb_latch_lock_state send: {e}"))?
            .check()
            .map_err(|e| format!("xkb_latch_lock_state check: {e}"))?;
        self.conn.flush().map_err(|e| format!("flush: {e}"))?;
        Ok(())
    }

    /// Эмулирует нажатие+отпускание keycode через XTest. Используется как
    /// fallback для ввода текста, когда uinput недоступен или мешает sandbox.
    fn fake_key(&self, keycode: u8) -> Result<(), String> {
        // XTest fake_input: type=KeyPress(2)/KeyRelease(3), detail=keycode.
        self.conn
            .xtest_fake_input(2, keycode, 0, self.root, 0, 0, 0)
            .map_err(|e| format!("xtest_fake_input press: {e}"))?
            .check()
            .map_err(|e| format!("xtest press check: {e}"))?;
        self.conn
            .xtest_fake_input(3, keycode, 0, self.root, 0, 0, 0)
            .map_err(|e| format!("xtest_fake_input release: {e}"))?
            .check()
            .map_err(|e| format!("xtest release check: {e}"))?;
        Ok(())
    }
}

/// Ленивый процесс-локальный singleton с переподключением при ошибке.
static BACKEND: Mutex<Option<Backend>> = Mutex::new(None);

fn with_backend<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&Backend) -> Result<R, String>,
{
    let mut guard = BACKEND.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = Some(Backend::connect()?);
    }
    let result = f(guard.as_ref().unwrap());
    if result.is_err() {
        // Сбрасываем соединение — на следующем вызове откроется заново.
        *guard = None;
    }
    result
}

/// Возвращает индекс активной XKB-группы (0..3).
pub fn current_group() -> Result<u8, String> {
    with_backend(|b| b.current_group())
}

/// Жёстко устанавливает заблокированную группу. Синхронно — все
/// последующие нажатия будут интерпретированы в новой раскладке.
pub fn lock_group(group: u8) -> Result<(), String> {
    with_backend(|b| b.lock_group(group))
}

/// Печатает X11-keycode (тот же, что и в evdev + 8 — стандартное смещение
/// между evdev keycodes и X11 keycodes). Без модификаторов.
pub fn fake_key(x11_keycode: u8) -> Result<(), String> {
    with_backend(|b| b.fake_key(x11_keycode))
}

/// Проверка доступности X11 backend. Возвращает Ok с описанием состояния
/// (для диагностики при старте daemon).
pub fn ping() -> Result<String, String> {
    let group = current_group()?;
    Ok(format!("X11 XKB ok, current_group={group}"))
}

/// Маппинг evdev keycode → X11 keycode (для XTest).
/// Linux evdev и X11 используют разные кодировки клавиш: X11 keycode = evdev + 8.
pub fn evdev_to_x11(evdev_keycode: u16) -> Option<u8> {
    let v = evdev_keycode as i32 + 8;
    if (0..=255).contains(&v) {
        Some(v as u8)
    } else {
        None
    }
}
