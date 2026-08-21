//! Natural (alphanumeric) string ordering for user-facing lists.
//!
//! Digit runs compare by numeric value, everything else ASCII-case-insensitively,
//! so "Bench 2" sorts before "Bench 10".

use std::cmp::Ordering;

/// Compare two strings in natural (alphanumeric) order.
///
/// Ties between numerically-equal digit runs ("01" vs "1") and
/// case-insensitively-equal text fall back to a plain byte comparison so the
/// ordering is total and stable.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a_chars = a.chars().peekable();
    let mut b_chars = b.chars().peekable();
    loop {
        match (a_chars.peek().copied(), b_chars.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let a_run = take_digit_run(&mut a_chars);
                    let b_run = take_digit_run(&mut b_chars);
                    let ordering = compare_digit_runs(&a_run, &b_run);
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                } else {
                    let fa = fold_char(ca);
                    let fb = fold_char(cb);
                    if fa != fb {
                        return fa.cmp(&fb);
                    }
                    a_chars.next();
                    b_chars.next();
                }
            }
        }
    }
}

fn fold_char(c: char) -> char {
    c.to_ascii_lowercase()
}

fn take_digit_run(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(&c) = chars.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        run.push(c);
        chars.next();
    }
    run
}

/// Numeric comparison of digit runs of arbitrary length (no overflow):
/// strip leading zeros, compare lengths, then lexicographically.
fn compare_digit_runs(a: &str, b: &str) -> Ordering {
    let a_trim = a.trim_start_matches('0');
    let b_trim = b.trim_start_matches('0');
    a_trim.len().cmp(&b_trim.len()).then_with(|| a_trim.cmp(b_trim))
}
