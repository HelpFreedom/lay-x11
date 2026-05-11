//! Маленькая диагностика X11-слоя: читает текущую группу, переключает и
//! читает обратно. Не пишет в /dev/uinput — только говорит с X-сервером.

use lay::x11_layout;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    println!("ping: {:?}", x11_layout::ping());

    let start = match x11_layout::current_group() {
        Ok(g) => {
            println!("текущая группа: {g}");
            g
        }
        Err(e) => {
            eprintln!("не удалось прочитать группу: {e}");
            return;
        }
    };

    let target: u8 = if start == 0 { 1 } else { 0 };
    println!("→ переключаю в группу {target}");
    if let Err(e) = x11_layout::lock_group(target) {
        eprintln!("ошибка lock_group: {e}");
        return;
    }
    sleep(Duration::from_millis(50));
    println!("после lock: {:?}", x11_layout::current_group());

    println!("→ возвращаю обратно в группу {start}");
    if let Err(e) = x11_layout::lock_group(start) {
        eprintln!("ошибка lock_group: {e}");
        return;
    }
    sleep(Duration::from_millis(50));
    println!("после restore: {:?}", x11_layout::current_group());
}
