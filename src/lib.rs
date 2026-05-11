//! lay — библиотечная часть. Используется и из `bin/lay` (CLI),
//! и из `bin/lay-daemon` (фоновый daemon на двойной Shift).

pub mod dict;
pub mod lem;
pub mod llm;
pub mod ngram;
pub mod quality;
pub mod stats;
pub mod x11_layout;
