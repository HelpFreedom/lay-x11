#!/bin/bash
# install.sh — собрать и установить lay + lay-daemon под X11
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

echo "=== проверка cargo ==="
if ! command -v cargo >/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        . "$HOME/.cargo/env"
    fi
fi
if ! command -v cargo >/dev/null; then
    echo "rust не установлен. Поставь:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y" >&2
    exit 1
fi
echo "✓ $(rustc --version)"

echo ""
echo "=== группа input (нужна для evdev) ==="
if id -nG "$USER" | grep -qw input; then
    echo "✓ уже в группе input"
else
    echo "→ добавляю $USER в группу input..."
    sudo usermod -aG input "$USER"
    echo "⚠ нужен перелогин чтобы группа применилась"
fi

echo ""
echo "=== пакеты системы ==="
need_apt=()
if [ ! -f /usr/share/hunspell/ru_RU.dic ]; then
    need_apt+=("hunspell-ru")
fi
if [ ${#need_apt[@]} -gt 0 ]; then
    echo "→ нужны для typing-assist (распознавание русских слов): ${need_apt[*]}"
    sudo apt-get install -y "${need_apt[@]}"
else
    echo "✓ hunspell-ru уже установлен"
fi
echo "  xdotool — опциональный fallback ввода текста (uinput работает почти всегда)"
if ! command -v xdotool >/dev/null; then
    echo "  ℹ xdotool не установлен; можно: sudo apt install xdotool"
fi

echo ""
echo "=== сборка release ==="
cargo build --release --quiet
echo "✓ lay:        $(ls -lh target/release/lay | awk '{print $5}')"
echo "✓ lay-daemon: $(ls -lh target/release/lay-daemon | awk '{print $5}')"

echo ""
echo "=== симлинки в ~/.local/bin/ ==="
mkdir -p ~/.local/bin
ln -sf "$DIR/target/release/lay" ~/.local/bin/lay
ln -sf "$DIR/target/release/lay-daemon" ~/.local/bin/lay-daemon
echo "✓ lay        → ~/.local/bin/lay"
echo "✓ lay-daemon → ~/.local/bin/lay-daemon"

echo ""
echo "=== systemd unit для lay-daemon ==="
mkdir -p ~/.config/systemd/user
cp "$DIR/systemd/lay-daemon.service" ~/.config/systemd/user/lay-daemon.service
systemctl --user daemon-reload
echo "✓ unit установлен; запусти когда будешь готов:"
echo "    systemctl --user enable --now lay-daemon"

echo ""
echo "=== пример-конфиги в ~/.config/lay/ ==="
mkdir -p ~/.config/lay
if [ ! -f ~/.config/lay/replacements.json ]; then
    cp "$DIR/examples/config/replacements.example.json" ~/.config/lay/replacements.json
    echo "✓ replacements.json создан из примера (бренды/акронимы/частые опечатки)"
else
    echo "✓ replacements.json уже существует — не трогаю"
fi
if [ ! -f ~/.config/lay/no_replace.txt ]; then
    cp "$DIR/examples/config/no_replace.example.txt" ~/.config/lay/no_replace.txt
    echo "✓ no_replace.txt создан из примера (команды терминала)"
else
    echo "✓ no_replace.txt уже существует — не трогаю"
fi

echo ""
echo "=== быстрый тест CLI ==="
echo "  Ye djn ghbvth → $(~/.local/bin/lay 'Ye djn ghbvth')"
echo "  руддщ цщкдв   → $(~/.local/bin/lay 'руддщ цщкдв')"

echo ""
echo "╔══════════════════════════════════════════╗"
echo "║  Установка завершена!                    ║"
echo "║                                          ║"
echo "║  Если группа input была добавлена —      ║"
echo "║  перелогинься перед запуском daemon.     ║"
echo "║                                          ║"
echo "║  Двойной Shift = конвертировать слово    ║"
echo "╚══════════════════════════════════════════╝"
