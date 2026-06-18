//! Held-out test suite — the HIDDEN, ADVERSARIAL scoring tests, owned by the EXAMINER.
//!
//! This file lives in `csv-examiner/`, deliberately OUTSIDE the proposing agent's
//! write scope (`csv-task/`). The agent never sees or runs these; the examiner runs
//! them to produce the held-out `k/N` score that actually counts.
//!
//! These tests are deliberately STRICT. An earlier 5-test suite was too lenient — a
//! visibly buggy parser (whitespace ignored, mid-field quotes mishandled) aced it
//! 5/5, which made the scoreboard useless: "5/5 held-out" meant "passed 5 gentle
//! tests", not "correct parser". The difficulty knob is the TEST SET, not the task,
//! so this suite targets the exact rules a naive `split(',')`-style parser gets
//! wrong, with a finer gradient (15 tests) so plan-execute and hillclimb have real
//! headroom to climb into.
//!
//! Expected values are grounded in RFC 4180 and cross-checked against Python's
//! `csv` module (the de-facto reference) for the cases RFC leaves implicit.
//!
//! Reference: RFC 4180, "Common Format and MIME Type for CSV Files".

use csv_task::parse_csv;

// ----------------------------------------------------------------------------
// Quoting (RFC §2.5–§2.7)
// ----------------------------------------------------------------------------

/// RFC 4180 §2.7 — a double-quote inside a quoted field is escaped by doubling it,
/// so `"a""b"` decodes to the single field `a"b`.
#[test]
fn escaped_double_quote() {
    assert_eq!(parse_csv("\"a\"\"b\"").unwrap(), vec![vec!["a\"b"]]);
}

/// RFC 4180 §2.5 — an empty quoted field `""` is the EMPTY STRING, not a literal
/// quote character. A naive parser that emits `"` or `""` here is wrong.
#[test]
fn empty_quoted_field_is_empty_string() {
    assert_eq!(parse_csv("\"\"").unwrap(), vec![vec![""]]);
}

/// RFC 4180 §2.5 — same rule mid-record: the empty quoted field between two commas
/// is an empty string, alongside its non-empty neighbours.
#[test]
fn empty_quoted_field_among_others() {
    assert_eq!(parse_csv("a,\"\",c").unwrap(), vec![vec!["a", "", "c"]]);
}

/// RFC 4180 §2.6 — a newline inside a quoted field is DATA, not a record
/// separator, so the input stays a single row with one field.
#[test]
fn newline_inside_quoted_field() {
    assert_eq!(parse_csv("\"a\nb\"").unwrap(), vec![vec!["a\nb"]]);
}

// ----------------------------------------------------------------------------
// Whitespace (RFC §2.4: "Spaces are considered part of a field and should not be
// ignored.") — the bug class the baseline parser left entirely unconsidered.
// ----------------------------------------------------------------------------

/// RFC 4180 §2.4 — leading/trailing spaces in UNQUOTED fields are part of the
/// field and must be preserved verbatim, NOT trimmed.
#[test]
fn preserves_unquoted_whitespace() {
    assert_eq!(parse_csv(" a , b ").unwrap(), vec![vec![" a ", " b "]]);
}

/// RFC 4180 §2.5/§2.6 — spaces INSIDE a quoted field are literal data; `" a "`
/// decodes to ` a ` with both spaces intact.
#[test]
fn preserves_whitespace_inside_quotes() {
    assert_eq!(parse_csv("\" a \"").unwrap(), vec![vec![" a "]]);
}

// ----------------------------------------------------------------------------
// Line endings (RFC §2.1–§2.2) — CRLF, LF, bare CR, and trailing-newline edges.
// ----------------------------------------------------------------------------

/// RFC 4180 §2.1 — records are delimited by CRLF. `a,b\r\nc,d` is two records,
/// and the carriage return must not leak into the first row's last field.
#[test]
fn crlf_record_separator() {
    assert_eq!(
        parse_csv("a,b\r\nc,d").unwrap(),
        vec![vec!["a", "b"], vec!["c", "d"]]
    );
}

/// RFC 4180 §2.1 — a CRLF is ONE separator, not two. Mixed `a\r\nb\nc` is three
/// records with NO spurious empty row between `a` and `b` (the classic bug from
/// splitting on `\r` and `\n` independently).
#[test]
fn mixed_crlf_and_lf() {
    assert_eq!(
        parse_csv("a\r\nb\nc").unwrap(),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );
}

/// De-facto (old-Mac) — a bare `\r` not followed by `\n` is a record separator, so
/// `a\rb` is two rows. A parser that treats it as data, or only handles `\n`/`\r\n`,
/// gets this wrong.
#[test]
fn bare_cr_record_separator() {
    assert_eq!(parse_csv("a\rb").unwrap(), vec![vec!["a"], vec!["b"]]);
}

/// RFC 4180 §2.2 — the last record may or may not end in a line break; a trailing
/// LF must NOT produce a spurious empty final record.
#[test]
fn trailing_lf_no_empty_row() {
    assert_eq!(parse_csv("a,b\n").unwrap(), vec![vec!["a", "b"]]);
}

/// RFC 4180 §2.2 — same rule for a trailing CRLF: `a,b\r\n` is exactly one record.
#[test]
fn trailing_crlf_no_empty_row() {
    assert_eq!(parse_csv("a,b\r\n").unwrap(), vec![vec!["a", "b"]]);
}

/// Empty input is zero records, not one empty row. `parse_csv("")` must be the
/// empty Vec — a parser that returns `[[""]]` for empty input is wrong.
#[test]
fn empty_input_no_rows() {
    let expected: Vec<Vec<String>> = vec![];
    assert_eq!(parse_csv("").unwrap(), expected);
}

// ----------------------------------------------------------------------------
// Malformed quoting must be rejected (RFC §2.5). The baseline parser silently
// accepted these (e.g. pushing a literal `"`); per RFC, quotes may not appear in
// or after an unquoted field, so a correct parser returns Err.
// ----------------------------------------------------------------------------

/// RFC 4180 §2.5 — "if fields are not enclosed with double quotes, then double
/// quotes may not appear inside the fields." A quote in the middle of an unquoted
/// field (`ab"c"d`) is malformed and must return `Err`, not a silently-accepted
/// field with literal quotes.
#[test]
fn quote_in_unquoted_field_is_error() {
    assert!(parse_csv("ab\"c\"d").is_err());
}

/// RFC 4180 §2.5 — a closed quoted field must be followed by a delimiter or line
/// break. Trailing text after the closing quote (`"a"b`) is malformed and must
/// return `Err`.
#[test]
fn text_after_closing_quote_is_error() {
    assert!(parse_csv("\"a\"b").is_err());
}

/// RFC 4180 §2.5 — a field opened with `"` must be closed with a matching `"`.
/// Reaching end-of-input inside an open quote (`"abc`) is malformed and must
/// return `Err`.
#[test]
fn unterminated_quote_is_error() {
    assert!(parse_csv("\"abc").is_err());
}
