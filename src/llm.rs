//! LLM-арбитр: на X11-сборке отключён.
//!
//! Файл содержит только заглушки тех же сигнатур, что и в оригинальной
//! GNOME-версии, чтобы остальной код daemon (smart-режим) компилировался.
//! Все функции возвращают «нет решения», и daemon скатывается в детерминированный
//! fallback. Smart-режим в этой сборке всегда ведёт себя как replay.

pub fn warm_up() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

pub fn model_backend_enabled() -> bool {
    false
}

pub fn convert_hybrid(
    _original: &str,
    _converted: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    Ok(None)
}

pub fn choose_token_hybrid(
    _original: &str,
    _converted: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    Ok(None)
}

pub fn choose_token_consensus(
    _original: &str,
    _converted: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    Ok(None)
}
