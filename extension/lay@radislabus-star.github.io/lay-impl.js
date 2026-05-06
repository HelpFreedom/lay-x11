/* lay-impl.js — реализация extension (меняется свободно, без logout)
 * Загружается через loader в extension.js с уникальным URL → нет кэша GJS.
 */

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {getInputSourceManager} from 'resource:///org/gnome/shell/ui/status/keyboard.js';

// ─── Config ────────────────────────────────────────────────

const CONFIG_PATH = GLib.get_home_dir() + '/.config/lay/config.json';
const STATS_PATH = GLib.get_home_dir() + '/.local/share/lay/stats.json';
const APP_VERSION = '0.1.117';
const APP_DESCRIPTION = 'Double Shift layout rescue for Linux/GNOME Wayland';
const APP_RELEASE_DATE = '2026-05-07';
const APP_LICENSE = 'MIT';
const APP_URL = 'https://github.com/radislabus-star/lay-public';
const APP_PLATFORM = 'GNOME Wayland';
const APP_GNOME_SUPPORT = 'GNOME 45-47, 50';
const MENU_WIDTH = 360;
const COMPACT_SUBTITLE_STYLE = 'font-weight:normal; font-size:76%; opacity:180;';
const SEGMENT_BUTTON_STYLE = 'padding:2px 8px; border-radius:6px; min-width:0;';
const LEARNING_LOG_TOOLTIP = 'Запоминать правки работает в два слоя:\n'
    + '• double-Shift пишет факт ручного исправления;\n'
    + '• после auto/smart lay ждёт до 30 секунд,\n'
    + '  удалишь ли ты результат и введёшь свой вариант.\n'
    + 'Если удалил и перепечатал — это считается твоей правкой.';
const DEFAULTS = {
    mode: 'simple',
    correction_engine: 'replay',
    trigger: 'double-lshift',
    tap_max_ms: 200,
    shift_window_ms: 250,
    debounce_ms: 50,
    replace_words: 1,
    auto_replace: false,
    typing_assist: false,
    learning_log: false,
};

function loadConfig() {
    try {
        const [, bytes] = Gio.File.new_for_path(CONFIG_PATH).load_contents(null);
        const parsed = JSON.parse(new TextDecoder().decode(bytes));
        const cfg = {...DEFAULTS, ...parsed};
        if (parsed.correction_engine === undefined)
            cfg.correction_engine = parsed.mode === 'llm' ? 'smart' : 'replay';
        return cfg;
    } catch(e) { return {...DEFAULTS}; }
}
function saveConfig(cfg) {
    try { Gio.File.new_for_path(GLib.get_home_dir() + '/.config/lay').make_directory_with_parents(null); } catch(e) {}
    const bytes = new TextEncoder().encode(JSON.stringify(cfg, null, 2));
    Gio.File.new_for_path(CONFIG_PATH).replace_contents(
        bytes, null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
}
function loadStats() {
    try {
        const [, bytes] = Gio.File.new_for_path(STATS_PATH).load_contents(null);
        return JSON.parse(new TextDecoder().decode(bytes));
    } catch(e) {
        return {};
    }
}
function restartDaemon() {
    daemonCommand('restart');
}
function startDaemon() {
    daemonCommand('start');
}
function stopDaemon() {
    daemonCommand('stop');
}
function daemonCommand(action) {
    try { Gio.Subprocess.new(['systemctl','--user',action,'lay-daemon'], Gio.SubprocessFlags.NONE); } catch(e) {}
}
function openUri(uri) {
    try {
        Gio.AppInfo.launch_default_for_uri(uri, global.create_app_launch_context(0, -1));
    } catch(e) {
        try { Gio.Subprocess.new(['xdg-open', uri], Gio.SubprocessFlags.NONE); } catch(_e) {}
    }
}

// ─── DBus ──────────────────────────────────────────────────

const DBUS_XML = `
<node>
  <interface name="io.github.radislabus_star.LayDaemon">
    <method name="Ping"><arg name="reply" direction="out" type="s"/></method>
    <method name="TypeText"><arg name="text" direction="in" type="s"/></method>
    <method name="ActivateLayout">
      <arg name="id" direction="in" type="s"/>
      <arg name="success" direction="out" type="b"/>
    </method>
    <method name="CurrentLayout"><arg name="id" direction="out" type="s"/></method>
    <method name="NextLayout"><arg name="success" direction="out" type="b"/></method>
    <method name="ListLayouts"><arg name="layouts" direction="out" type="s"/></method>
  </interface>
</node>`;

const DBUS_PATH = '/io/github/radislabus_star/LayDaemon';

class LayDaemonService {
    enable() {
        const seat = Clutter.get_default_backend().get_default_seat();
        this._vdev = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        this._dbus = Gio.DBusExportedObject.wrapJSObject(DBUS_XML, this);
        this._dbus.export(Gio.DBus.session, DBUS_PATH);
        log('[lay-extension] DBus enabled');
    }
    disable() {
        this._dbus?.unexport(); this._dbus = null; this._vdev = null;
        log('[lay-extension] DBus disabled');
    }
    Ping() { return 'pong from lay-extension'; }
    TypeText(text) {
        if (Main.inputMethod?.commit) { try { Main.inputMethod.commit(text); return; } catch(e) {} }
        this._typeTextByKeyvals(text);
    }
    _tapKeyval(keyval, count) {
        for (let i = 0; i < count; i++) {
            this._vdev?.notify_keyval(Clutter.CURRENT_TIME, keyval, Clutter.KeyState.PRESSED);
            this._vdev?.notify_keyval(Clutter.CURRENT_TIME, keyval, Clutter.KeyState.RELEASED);
        }
    }
    _typeTextByKeyvals(text) {
        for (const ch of text) {
            const kv = Clutter.unicode_to_keysym(ch.codePointAt(0));
            if (!kv) continue;
            this._tapKeyval(kv, 1);
        }
    }
    ActivateLayout(id) {
        try {
            const mgr = getInputSourceManager();
            for (const i in mgr.inputSources)
                if (mgr.inputSources[i].id === id) { mgr.inputSources[i].activate(); return true; }
        } catch(e) {}
        return false;
    }
    CurrentLayout() {
        try {
            return getInputSourceManager().currentSource?.id ?? '';
        } catch(e) { return ''; }
    }
    NextLayout() {
        try {
            const mgr = getInputSourceManager();
            const ids = Object.keys(mgr.inputSources).sort((a,b)=>a-b);
            const cur = ids.findIndex(i => mgr.inputSources[i].id === mgr.currentSource.id);
            mgr.inputSources[ids[(cur+1)%ids.length]].activate();
            return true;
        } catch(e) { return false; }
    }
    ListLayouts() {
        try {
            const mgr = getInputSourceManager();
            return Object.keys(mgr.inputSources).sort((a,b)=>a-b)
                .map(i=>`${i}:${mgr.inputSources[i].type}:${mgr.inputSources[i].id}${mgr.inputSources[i].id===mgr.currentSource.id?'*':''}`)
                .join(',');
        } catch(e) { return 'error:'+e; }
    }
}

// ─── Tray Indicator ────────────────────────────────────────
// Уникальный GTypeName предотвращает ошибку "already registered"
// при повторном disable→enable в одной сессии.

const _uid = Date.now();

const LayIndicator = GObject.registerClass(
{GTypeName: `LayIndicator_${_uid}`},
class LayIndicator extends PanelMenu.Button {

    _init() {
        super._init(0.0, 'lay');
        this._cfg = loadConfig();
        this._cfg.replace_words = Math.max(1, Math.min(2, this._cfg.replace_words));
        this._cfg.correction_engine = this._cfg.correction_engine === 'smart' ? 'smart' : 'replay';

        this._panelBox = new St.BoxLayout({
            style: 'spacing:4px; padding:0 2px;',
        });
        this._panelIcon = new St.Icon({
            icon_name: 'input-keyboard-symbolic',
            style_class: 'system-status-icon',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._label = new St.Label({
            text: '--',
            y_align: Clutter.ActorAlign.CENTER,
            style: 'font-weight:bold; padding:0 2px;',
        });
        this._panelBox.add_child(this._panelIcon);
        this._panelBox.add_child(this._label);
        this.add_child(this._panelBox);

        this._buildMenu();
        this.menu.connect('open-state-changed', (_menu, isOpen) => {
            if (isOpen)
                this._refreshStats();
        });

        this._mgr = getInputSourceManager();
        this._srcId = this._mgr.connect('current-source-changed', () => this._refreshLayout());
        this._refreshLayout();
    }

    _buildMenu() {
        this.menu.box.style = `min-width:${MENU_WIDTH}px; padding:2px 0;`;
        this._engineButtons = {};
        this._scopeButtons = {};
        this._triggerButtons = {};
        this._triggerItems = {};
        this._toggleButtons = {};
        this._statusRefreshIds = [];

        this._statusItem = this._headerItem();
        this.menu.addMenuItem(this._statusItem);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this.menu.addMenuItem(this._switchItem('Помощь при наборе', 'typing_assist', true));
        this.menu.addMenuItem(this._switchItem('Автоподмена', 'auto_replace', true));
        this.menu.addMenuItem(this._switchItem(
            'Запоминать правки',
            'learning_log',
            false,
            LEARNING_LOG_TOOLTIP
        ));
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this.menu.addMenuItem(this._segmentedRow('Режим', [
            ['replay', 'Replay', () => {
                this._cfg.correction_engine = 'replay';
                this._cfg.mode = 'simple';
                this._saveAndRefresh();
            }],
            ['smart', 'Smart', () => {
                this._cfg.correction_engine = 'smart';
                this._cfg.mode = 'simple';
                this._saveAndRefresh();
            }],
        ], this._engineButtons));

        this.menu.addMenuItem(this._segmentedRow('Область', [
            ['1', '1 слово', () => {
                this._cfg.replace_words = 1;
                this._saveAndRefresh();
            }],
            ['2', '2 слова', () => {
                this._cfg.replace_words = 2;
                this._saveAndRefresh();
            }],
        ], this._scopeButtons));

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addMenuItem(this._triggerMenu());
        this.menu.addMenuItem(this._timingMenu());
        this.menu.addMenuItem(this._daemonSwitchItem());
        this.menu.addMenuItem(this._aboutMenu());

        this._refreshSelections();
        this._refreshStatus();
    }

    _headerItem() {
        const item = new PopupMenu.PopupBaseMenuItem({activate: false, reactive: false, can_focus: false});
        item.reactive = false;
        item.can_focus = false;
        item.style = 'padding:5px 12px 4px 12px;';
        const card = new St.BoxLayout({
            x_expand: true,
            style: 'spacing:8px;',
        });
        const icon = new St.Icon({
            icon_name: 'input-keyboard-symbolic',
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'popup-menu-icon',
        });
        const titleBox = new St.BoxLayout({x_expand: true, style: 'spacing:6px;'});
        const title = new St.Label({
            text: `Lay ${APP_VERSION}`,
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: true,
            style: 'font-weight:bold;',
        });
        this._statusLabel = new St.Label({
            text: 'проверка...',
            y_align: Clutter.ActorAlign.CENTER,
            style: COMPACT_SUBTITLE_STYLE,
        });
        this._statusDot = new St.Label({
            text: '●',
            y_align: Clutter.ActorAlign.CENTER,
            style: 'font-size:90%; color:#f6c343;',
        });
        titleBox.add_child(title);
        titleBox.add_child(this._statusLabel);
        card.add_child(icon);
        card.add_child(titleBox);
        card.add_child(this._statusDot);
        item.add_child(card);
        return item;
    }

    _switchItem(label, key, restart = false, tooltip = null) {
        const item = new PopupMenu.PopupSwitchMenuItem(label, !!this._cfg[key], {});
        item.connect('toggled', (_item, state) => {
            this._cfg[key] = state;
            this._saveAndRefresh();
            if (restart) {
                restartDaemon();
                this._setDaemonBusy('restarting...');
                this._scheduleStatusRefreshes();
            }
        });
        if (tooltip)
            this._attachTooltip(item, tooltip);
        this._toggleButtons[key] = item;
        return item;
    }

    _attachTooltip(actor, text) {
        actor.connect('enter-event', () => {
            this._showTooltip(actor, text);
            return Clutter.EVENT_PROPAGATE;
        });
        actor.connect('leave-event', () => {
            this._hideTooltip();
            return Clutter.EVENT_PROPAGATE;
        });
    }

    _showTooltip(anchor, text) {
        this._hideTooltip();

        const tooltip = new St.Label({
            text,
            style_class: 'dash-label',
            style: 'padding:8px 10px; border:1px solid rgba(255,255,255,0.28); border-radius:8px;',
        });
        tooltip.width = 420;
        tooltip.clutter_text.line_wrap = true;
        tooltip.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
        Main.uiGroup.add_child(tooltip);

        const [x, y] = anchor.get_transformed_position();
        const [width] = anchor.get_transformed_size();
        const [, tooltipWidth] = tooltip.get_preferred_width(-1);
        const [, tooltipHeight] = tooltip.get_preferred_height(tooltipWidth);
        let tx = x + width + 8;
        if (tx + tooltipWidth > global.stage.width - 8)
            tx = Math.max(8, x - tooltipWidth - 8);
        const ty = Math.max(8, Math.min(y - 2, global.stage.height - tooltipHeight - 8));
        tooltip.set_position(Math.round(tx), Math.round(ty));
        tooltip.opacity = 255;
        this._tooltip = tooltip;
    }

    _hideTooltip() {
        if (!this._tooltip)
            return;
        this._tooltip.destroy();
        this._tooltip = null;
    }

    _segmentedRow(title, options, target) {
        const item = new PopupMenu.PopupBaseMenuItem({activate: false, reactive: false, can_focus: false});
        item.reactive = false;
        item.can_focus = false;
        item.style = 'padding:4px 12px;';

        const label = new St.Label({
            text: title,
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: true,
            style: 'font-weight:bold;',
        });
        item.add_child(label);

        const controls = new St.BoxLayout({style: 'spacing:4px;'});
        for (const [id, text, onClick] of options) {
            const button = new St.Button({
                label: text,
                reactive: true,
                can_focus: true,
                toggle_mode: true,
                style_class: 'button flat',
                style: SEGMENT_BUTTON_STYLE,
            });
            button.connect('clicked', onClick);
            target[id] = button;
            controls.add_child(button);
        }
        item.add_child(controls);
        return item;
    }

    _triggerMenu() {
        const item = new PopupMenu.PopupSubMenuMenuItem('', false);
        this._triggerMenuItem = item;
        for (const [id, label] of [
            ['double-lshift', 'Double Shift'],
            ['double-ctrl', 'Ctrl×2'],
            ['double-alt', 'Alt×2'],
            ['caps-lock', 'CapsLock'],
            ['single-rshift', 'RShift'],
            ['single-rctrl', 'RCtrl'],
            ['single-ralt', 'RAlt'],
        ]) {
            const row = new PopupMenu.PopupMenuItem(label);
            row.connect('activate', () => this._setTrigger(id));
            this._triggerItems[id] = row;
            item.menu.addMenuItem(row);
        }
        return item;
    }

    _timingMenu() {
        const item = new PopupMenu.PopupSubMenuMenuItem('Тайминг', false);
        item.menu.addMenuItem(this._timingCompactRow('Тап', 'tap_max_ms', 'мс', [100,150,200,250,300,350,400]));
        item.menu.addMenuItem(this._timingCompactRow('Окно', 'shift_window_ms', 'мс', [150,200,250,300,400,500]));
        return item;
    }

    _daemonSwitchItem() {
        const item = new PopupMenu.PopupSwitchMenuItem('Daemon', false, {});
        item.connect('toggled', (_item, state) => {
            if (this._updatingDaemonSwitch)
                return;
            this._toggleDaemonService(state);
        });
        this._daemonSwitch = item;
        return item;
    }

    _aboutMenu() {
        const item = new PopupMenu.PopupSubMenuMenuItem('О программе', false);
        const block = new PopupMenu.PopupBaseMenuItem({activate: false, reactive: false, can_focus: false});
        block.reactive = false;
        block.can_focus = false;
        block.style = 'padding:8px 12px 10px 12px;';

        const box = new St.BoxLayout({
            vertical: true,
            x_expand: true,
            style: 'spacing:3px;',
        });
        box.add_child(new St.Label({
            text: `Lay ${APP_VERSION}`,
            style: 'font-weight:bold;',
        }));
        box.add_child(new St.Label({
            text: APP_DESCRIPTION,
            style: COMPACT_SUBTITLE_STYLE,
        }));
        box.add_child(new St.Label({
            text: `Дата версии: ${APP_RELEASE_DATE}`,
            style: COMPACT_SUBTITLE_STYLE,
        }));
        box.add_child(new St.Label({
            text: APP_PLATFORM,
            style: COMPACT_SUBTITLE_STYLE,
        }));
        box.add_child(new St.Label({
            text: `Совместимость: ${APP_GNOME_SUPPORT}`,
            style: COMPACT_SUBTITLE_STYLE,
        }));
        box.add_child(new St.Label({
            text: `Лицензия: ${APP_LICENSE}`,
            style: COMPACT_SUBTITLE_STYLE,
        }));
        this._aboutConfigLabel = new St.Label({
            text: `Настройки: ${this._aboutConfigText()}`,
            style: COMPACT_SUBTITLE_STYLE,
        });
        box.add_child(this._aboutConfigLabel);
        this._aboutStatsLabel = new St.Label({
            text: `Статистика: ${this._aboutStatsText()}`,
            style: COMPACT_SUBTITLE_STYLE,
        });
        box.add_child(this._aboutStatsLabel);
        const link = new St.Label({
            text: APP_URL,
            reactive: true,
            can_focus: true,
            style: `${COMPACT_SUBTITLE_STYLE}; text-decoration: underline;`,
        });
        link.connect('button-release-event', () => {
            openUri(APP_URL);
            return Clutter.EVENT_STOP;
        });
        box.add_child(link);
        block.add_child(box);
        item.menu.addMenuItem(block);
        return item;
    }

    _timingCompactRow(title, key, suffix, steps) {
        const item = new PopupMenu.PopupBaseMenuItem({activate: false, reactive: false, can_focus: false});
        item.reactive = false;
        item.can_focus = false;
        item.style = 'padding:4px 12px;';
        item.add_child(new St.Label({
            text: title,
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: true,
        }));

        const value = new St.Label({
            text: `${this._cfg[key]}${suffix}`,
            y_align: Clutter.ActorAlign.CENTER,
            style: 'font-feature-settings:"tnum";',
        });
        const controls = new St.BoxLayout({style: 'spacing:4px;'});
        controls.add_child(this._textStepButton('−', () => this._stepTiming(key, steps, -1, value, suffix)));
        controls.add_child(value);
        controls.add_child(this._textStepButton('+', () => this._stepTiming(key, steps, 1, value, suffix)));
        item.add_child(controls);
        return item;
    }

    _textStepButton(label, onClick) {
        const button = new St.Button({
            label,
            reactive: true,
            can_focus: true,
            style_class: 'button flat',
            style: 'padding:1px 7px; border-radius:999px; min-width:0;',
        });
        button.connect('clicked', onClick);
        return button;
    }

    _stepTiming(key, steps, delta, value, suffix) {
        const idx = steps.indexOf(this._cfg[key]);
        const ni = Math.max(0, Math.min(steps.length - 1, idx + delta));
        if (ni === idx)
            return;
        this._cfg[key] = steps[ni];
        value.text = `${this._cfg[key]}${suffix}`;
        saveConfig(this._cfg);
        restartDaemon();
    }

    _refreshSelections() {
        if (this._triggerMenuItem)
            this._triggerMenuItem.label.text = `Триггер: ${this._triggerLabel(this._cfg.trigger)}`;
        if (this._aboutConfigLabel)
            this._aboutConfigLabel.text = `Настройки: ${this._aboutConfigText()}`;
        this._refreshStats();
        for (const [id, button] of Object.entries(this._engineButtons ?? {}))
            this._setButtonActive(button, id === this._cfg.correction_engine);
        for (const [id, button] of Object.entries(this._scopeButtons ?? {}))
            this._setButtonActive(button, Number(id) === this._cfg.replace_words);
        for (const [id, button] of Object.entries(this._triggerButtons ?? {}))
            this._setButtonActive(button, id === this._cfg.trigger);
        for (const [id, row] of Object.entries(this._triggerItems ?? {}))
            row.setOrnament(id === this._cfg.trigger ? PopupMenu.Ornament.CHECK : PopupMenu.Ornament.NONE);
        for (const [key, button] of Object.entries(this._toggleButtons ?? {})) {
            if (button.setToggleState)
                button.setToggleState(!!this._cfg[key]);
            else
                this._setButtonActive(button, !!this._cfg[key]);
        }
    }

    _saveAndRefresh() {
        this._cfg.replace_words = Math.max(1, Math.min(2, this._cfg.replace_words));
        this._cfg.correction_engine = this._cfg.correction_engine === 'smart' ? 'smart' : 'replay';
        this._cfg.mode = 'simple';
        this._refreshSelections();
        saveConfig(this._cfg);
    }

    _setTrigger(id) {
        if (this._cfg.trigger === id) {
            this._refreshSelections();
            return;
        }
        this._cfg.trigger = id;
        this._saveAndRefresh();
        restartDaemon();
        this._setDaemonBusy('restarting...');
        this._scheduleStatusRefreshes();
    }

    _toggleDaemonService(shouldStart = null) {
        if (shouldStart === null)
            shouldStart = this._daemonActive === false;
        if (shouldStart) {
            startDaemon();
            this._setDaemonBusy('starting...');
        } else {
            stopDaemon();
            this._setDaemonBusy('stopping...');
        }
        this._scheduleStatusRefreshes();
    }

    _setButtonActive(button, active) {
        button.set_style_class_name(active ? 'button' : 'button flat');
        button.style = SEGMENT_BUTTON_STYLE;
        if (button.set_checked)
            button.set_checked(active);
    }

    _triggerLabel(id) {
        return {
            'double-lshift': 'Double Shift',
            'double-ctrl': 'Ctrl×2',
            'double-alt': 'Alt×2',
            'caps-lock': 'CapsLock',
            'single-rshift': 'RShift',
            'single-rctrl': 'RCtrl',
            'single-ralt': 'RAlt',
        }[id] ?? 'Double Shift';
    }

    _aboutConfigText() {
        return `${this._engineLabel()} · ${this._cfg.replace_words} сл. · ${this._triggerLabel(this._cfg.trigger)}`;
    }

    _aboutStatsText() {
        const stats = loadStats();
        return `LLM ${stats.llm_calls ?? 0}${this._lastTime(stats.last_llm_ts)} · `
            + `правки ${stats.learning_log_entries ?? 0}${this._lastTime(stats.last_learning_ts)} · `
            + `правил ${stats.promoted_rules ?? 0}${this._lastTime(stats.last_promotion_ts)}`;
    }

    _refreshStats() {
        if (this._aboutStatsLabel)
            this._aboutStatsLabel.text = `Статистика: ${this._aboutStatsText()}`;
    }

    _lastTime(ts) {
        if (!ts)
            return '';
        try {
            const date = new Date(ts * 1000);
            return `, ${date.toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})}`;
        } catch(e) {
            return '';
        }
    }

    _engineLabel() {
        return this._cfg.correction_engine === 'smart' ? 'Smart' : 'Replay';
    }

    _refreshLayout() {
        try {
            const isRu = this._mgr.currentSource?.id === 'ru';
            this._label.text = isRu ? 'RU' : 'EN';
        } catch(e) { this._label.text = '--'; }
    }

    _refreshStatus() {
        try {
            const p = Gio.Subprocess.new(
                ['systemctl','--user','is-active','lay-daemon'],
                Gio.SubprocessFlags.STDOUT_PIPE);
            p.communicate_utf8_async(null, null, (proc, res) => {
                try {
                    const [, out] = proc.communicate_utf8_finish(res);
                    const ok = out.trim() === 'active';
                    this._daemonActive = ok;
                    this._statusLabel.text = ok ? 'daemon active' : 'daemon stopped';
                    this._setDaemonStatus(ok);
                    this._refreshDaemonAction(ok);
                } catch(e) {}
            });
        } catch(e) {}
    }

    _setDaemonBusy(text) {
        this._stopStatusBlink();
        if (this._statusLabel)
            this._statusLabel.text = text;
        if (this._statusDot) {
            this._statusDot.opacity = 255;
            this._statusDot.style = 'font-size:90%; color:#f6c343;';
        }
    }

    _refreshDaemonAction(active) {
        if (this._daemonSwitch?.setToggleState) {
            this._updatingDaemonSwitch = true;
            this._daemonSwitch.setToggleState(active);
            this._updatingDaemonSwitch = false;
        }
    }

    _scheduleStatusRefreshes() {
        this._clearStatusRefreshes();
        for (const delay of [700, 1500, 3000]) {
            const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, delay, () => {
                this._statusRefreshIds = this._statusRefreshIds.filter(existing => existing !== id);
                this._refreshStatus();
                return false;
            });
            this._statusRefreshIds.push(id);
        }
    }

    _clearStatusRefreshes() {
        for (const id of this._statusRefreshIds ?? [])
            GLib.Source.remove(id);
        this._statusRefreshIds = [];
    }

    _setDaemonStatus(active) {
        if (!this._statusDot)
            return;

        if (active) {
            this._statusDot.style = 'font-size:90%; color:#26a269;';
            this._startStatusBlink();
        } else {
            this._stopStatusBlink();
            this._statusDot.opacity = 255;
            this._statusDot.style = 'font-size:90%; color:#c01c28;';
        }
    }

    _startStatusBlink() {
        if (this._blinkId)
            return;

        this._blinkBright = true;
        this._statusDot.opacity = 255;
        this._blinkId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 650, () => {
            if (!this._statusDot) {
                this._blinkId = 0;
                return false;
            }
            this._blinkBright = !this._blinkBright;
            this._statusDot.opacity = this._blinkBright ? 255 : 95;
            return true;
        });
    }

    _stopStatusBlink() {
        if (this._blinkId) {
            GLib.Source.remove(this._blinkId);
            this._blinkId = 0;
        }
    }

    destroy() {
        this._clearStatusRefreshes();
        this._stopStatusBlink();
        this._hideTooltip();
        if (this._srcId) { this._mgr.disconnect(this._srcId); this._srcId = 0; }
        super.destroy();
    }
});

// ─── Entry point ───────────────────────────────────────────

export class LayImpl {
    constructor(_ext) {}

    enable() {
        this._service = new LayDaemonService();
        this._service.enable();
        this._indicator = new LayIndicator();
        Main.panel.addToStatusArea(`lay-${_uid}`, this._indicator, 0, 'right');
        log('[lay-extension] LayImpl enabled ✓');
    }

    disable() {
        this._indicator?.destroy(); this._indicator = null;
        this._service?.disable();   this._service   = null;
    }
}
