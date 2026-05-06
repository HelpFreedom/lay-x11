//! Оценка качества словарной конвертации.
//!
//! Простая эвристика без внешних зависимостей: считаем долю слов,
//! которые "выглядят как настоящие". Для русского/английского
//! проверяем что в слове нет редких сочетаний согласных подряд
//! (типичный признак случайного набора в чужой раскладке).
//!
//! Это компромисс между качеством детекта и весом бинарника:
//! полные словари hunspell весят 10-30 МБ, а нам нужен мгновенный
//! ответ "плохой ли результат".

const RU_VOWELS: &str = "аеёиоуыэюяАЕЁИОУЫЭЮЯ";
const EN_VOWELS: &str = "aeiouAEIOU";

/// Доля «правдоподобных» слов в тексте (0..1).
pub fn score(text: &str, lang: &str) -> f32 {
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return 1.0;
    }

    let plausible = words.iter().filter(|w| is_plausible_word(w, lang)).count();
    plausible as f32 / words.len() as f32
}

fn is_plausible_word(word: &str, lang: &str) -> bool {
    let len = word.chars().count();

    // Слова длиной 1-2 символа всегда считаем плауcибельными
    if len <= 2 {
        return true;
    }

    let vowels = match lang {
        "ru" => RU_VOWELS,
        _ => EN_VOWELS,
    };
    let vowel_count = word.chars().filter(|c| vowels.contains(*c)).count();

    // 1. Хотя бы одна гласная (русский: 1 на 6 символов, английский: 1 на 5)
    let min_vowels = match lang {
        "ru" => (len as f32 / 6.0).ceil() as usize,
        _ => (len as f32 / 5.0).ceil() as usize,
    }
    .max(1);
    if vowel_count < min_vowels {
        return false;
    }

    // 2. Нет 4+ согласных подряд
    let mut consonant_streak = 0;
    for c in word.chars() {
        if c.is_alphabetic() && !vowels.contains(c) {
            consonant_streak += 1;
            if consonant_streak >= 4 {
                return false;
            }
        } else {
            consonant_streak = 0;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_russian_text() {
        // Реальные русские слова
        let s = score("Ну вот пример хорошего текста", "ru");
        assert!(s > 0.8, "score = {s}");
    }

    #[test]
    fn bad_russian_text() {
        // Случайный набор кириллицы (как если бы английский набрали в RU)
        // Эвристика без словаря даёт ~0.5 — этого достаточно для трешхолда 0.7
        let s = score("руддщ цщкдв", "ru");
        assert!(s < 0.7, "score = {s}");
    }

    #[test]
    fn good_english_text() {
        let s = score("hello world this is fine", "en");
        assert!(s > 0.8, "score = {s}");
    }

    #[test]
    fn bad_english_text() {
        // Русский в английской раскладке
        let s = score("Ye djn ghbvth", "en");
        assert!(s < 0.5, "score = {s}");
    }
}
