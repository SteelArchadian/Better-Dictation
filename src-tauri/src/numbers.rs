//! Spoken numbers to digits, only where it is clearly a number: "thirty five" → 35,
//! "twenty twenty six" → 2026, "two hundred and five" → 205. Small numbers on their own
//! ("one of the best", "fifteen minutes") and phrases like "a hundred percent" or "five forty
//! five" stay as words, since a wrong guess there reads worse than words.

const UNITS: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
    "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
];
const TENS: [&str; 8] = ["twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
const ORDINALS: [&str; 9] = ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth"];

enum Part {
    Unit(u64),
    Tens(u64),
    Scale(u64),
}

fn part(w: &str) -> Option<Part> {
    if let Some(i) = UNITS.iter().position(|u| *u == w) {
        return Some(Part::Unit(i as u64));
    }
    if let Some(i) = TENS.iter().position(|t| *t == w) {
        return Some(Part::Tens(20 + 10 * i as u64));
    }
    match w {
        "hundred" => Some(Part::Scale(100)),
        "thousand" => Some(Part::Scale(1_000)),
        "million" => Some(Part::Scale(1_000_000)),
        _ => None,
    }
}

fn core(w: &str) -> String {
    w.trim_end_matches([',', '.', '?', '!']).to_lowercase()
}

fn pieces(core: &str) -> Vec<&str> {
    core.split('-').collect()
}

pub fn is_number_word(w: &str) -> bool {
    let c = core(w);
    pieces(&c).iter().all(|p| part(p).is_some())
}

/// For each word: None keeps it; Some(text) replaces it, with "" for words folded into the
/// number before them. The run's last punctuation stays on the number.
pub fn digits(words: &[String]) -> Vec<Option<String>> {
    let mut out = vec![None; words.len()];
    let mut i = 0;
    while i < words.len() {
        let mut j = i;
        while j < words.len() {
            let ends_clause = words[j].ends_with([',', '.', '?', '!']);
            let c = core(&words[j]);
            // "and" only inside a number: "two hundred and five".
            let joins = c == "and"
                && j > i
                && matches!(part(&core(&words[j - 1])), Some(Part::Scale(_)))
                && words.get(j + 1).is_some_and(|n| is_number_word(n))
                && !words[j - 1].ends_with([',', '.', '?', '!']);
            if !(is_number_word(&words[j]) || joins) {
                break;
            }
            j += 1;
            if ends_clause {
                break;
            }
        }
        if j == i {
            i += 1;
            continue;
        }
        let ordinal_next = words.get(j).is_some_and(|w| ORDINALS.contains(&core(w).as_str()));
        let cores: Vec<String> = words[i..j].iter().map(|w| core(w)).collect();
        if let Some(n) = (!ordinal_next).then(|| convert(&cores)).flatten() {
            let punct: String = words[j - 1].chars().rev().take_while(|c| ",.?!".contains(*c)).collect();
            out[i] = Some(n + &punct.chars().rev().collect::<String>());
            for slot in &mut out[i + 1..j] {
                *slot = Some(String::new());
            }
        }
        i = j;
    }
    out
}

fn convert(cores: &[String]) -> Option<String> {
    let parts: Vec<Part> =
        cores.iter().filter(|c| *c != "and").flat_map(|c| pieces(c).into_iter().map(part).collect::<Vec<_>>()).collect::<Option<_>>()?;
    if parts.iter().any(|p| matches!(p, Part::Scale(_))) {
        return scaled(&parts).map(group_thousands);
    }
    // Without hundred/thousand: split into two-digit groups ("thirty five" is one group).
    let mut groups = Vec::new();
    let mut k = 0;
    while k < parts.len() {
        match (&parts[k], parts.get(k + 1)) {
            (Part::Tens(t), Some(Part::Unit(u))) if (1..=9).contains(u) => {
                groups.push(t + u);
                k += 2;
            }
            (Part::Tens(t), _) => {
                groups.push(*t);
                k += 1;
            }
            (Part::Unit(u), _) => {
                groups.push(*u);
                k += 1;
            }
            (Part::Scale(_), _) => return None,
        }
    }
    match groups[..] {
        [g] if g >= 20 || parts.len() > 1 => Some(g.to_string()),
        // Years: "nineteen eighty four", "twenty twenty six".
        [a, b] if (11..=99).contains(&a) && (10..=99).contains(&b) => Some(format!("{a}{b}")),
        _ => None,
    }
}

/// "two thousand five hundred and six" → 2506. A scale needs a number before it.
fn scaled(parts: &[Part]) -> Option<u64> {
    let (mut total, mut current) = (0u64, 0u64);
    let mut last_scale = u64::MAX;
    for p in parts {
        match p {
            Part::Unit(u) if current % 10 == 0 && (current % 100 == 0 || *u < 10) => current += u,
            Part::Tens(t) if current % 100 == 0 => current += t,
            Part::Scale(100) if (1..=99).contains(&current) => current *= 100,
            Part::Scale(s) if *s > 100 && current > 0 && *s < last_scale => {
                total += current * s;
                current = 0;
                last_scale = *s;
            }
            _ => return None,
        }
    }
    Some(total + current)
}

fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    if n < 10_000 {
        return s;
    }
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str) -> String {
        let words: Vec<String> = text.split(' ').map(String::from).collect();
        words
            .iter()
            .zip(digits(&words))
            .filter_map(|(w, d)| match d {
                Some(d) if d.is_empty() => None,
                Some(d) => Some(d),
                None => Some(w.clone()),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn clear_numbers_become_digits() {
        assert_eq!(apply("the biggest fix rewound thirty characters,"), "the biggest fix rewound 30 characters,");
        assert_eq!(apply("about forty-five people"), "about 45 people");
        assert_eq!(apply("thirty five apples."), "35 apples.");
        assert_eq!(apply("in twenty twenty six"), "in 2026");
        assert_eq!(apply("nineteen eighty four"), "1984");
        assert_eq!(apply("two hundred and five"), "205");
        assert_eq!(apply("two thousand five hundred"), "2500");
        assert_eq!(apply("twelve thousand three hundred"), "12,300");
    }

    #[test]
    fn unclear_numbers_stay_words() {
        for text in [
            "one of the best",
            "fifteen minutes",
            "a hundred percent",
            "a thousand times",
            "five forty five",
            "the thirty first",
            "one two three",
            "rock and roll",
        ] {
            assert_eq!(apply(text), text);
        }
    }
}
