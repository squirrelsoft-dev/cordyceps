//! Dev test suite — the VISIBLE feedback tests that DRIVE the climb.
//!
//! HillClimbing scores each iteration against this set (the keep-iff-better gate),
//! and the agent can run it for feedback while it iterates. So this set has two
//! jobs: it must be hard enough that a single shot does NOT ace it (otherwise the
//! climb has no gradient — there's nothing left to improve), and it must mirror
//! the held-out skills so that climbing here generalizes to the blind scoreboard.
//!
//! ## The skill-mirroring rule
//!
//! Every test here mirrors a skill exercised by the HIDDEN held-out suite
//! (`csv-examiner/tests/heldout.rs`) — but with DIFFERENT literal inputs. Same
//! skill, different data. That keeps the two sets DISJOINT, so held-out stays a
//! true blind generalization check: an implementation that overfits to these exact
//! dev inputs (instead of learning the rule) climbs dev while held-out stays flat,
//! and that divergence is the gaming signal. When adding a dev test, add the skill,
//! not a copy of a held-out input.
//!
//! Reference: RFC 4180, "Common Format and MIME Type for CSV Files".

use csv_task::parse_csv;

// --- basics: comma/record structure -----------------------------------------

/// RFC 4180 §2.4 — fields are comma-separated; a plain record of unquoted fields.
#[test]
fn simple_row() {
    assert_eq!(parse_csv("a,b,c").unwrap(), vec![vec!["a", "b", "c"]]);
}

/// RFC 4180 §2.1 — records on separate lines (LF here; held-out mirrors this with
/// CRLF / mixed endings).
#[test]
fn rows_split_on_newline() {
    assert_eq!(
        parse_csv("a,b\nc,d").unwrap(),
        vec![vec!["a", "b"], vec!["c", "d"]]
    );
}

/// RFC 4180 §2.4 — adjacent commas denote an empty UNQUOTED field (distinct from
/// the empty QUOTED field skill below / in held-out).
#[test]
fn empty_unquoted_field() {
    assert_eq!(parse_csv("a,,c").unwrap(), vec![vec!["a", "", "c"]]);
}

/// RFC 4180 §2.6 — a comma inside a quoted field is data, not a delimiter.
#[test]
fn quoted_field_with_comma() {
    assert_eq!(parse_csv("\"a,b\",c").unwrap(), vec![vec!["a,b", "c"]]);
}

// --- mirrored skills: same rules as held-out, different inputs ----------------

/// RFC 4180 §2.7 — doubled quote inside a quoted field is one literal quote.
/// (held-out mirrors with `"a""b"`; here `"p""q"` → `p"q`.)
#[test]
fn escaped_double_quote() {
    assert_eq!(parse_csv("\"p\"\"q\"").unwrap(), vec![vec!["p\"q"]]);
}

/// RFC 4180 §2.4 — "Spaces are considered part of a field and should not be
/// ignored." Unquoted whitespace is preserved. (held-out: ` a , b `; here
/// ` x , y `.)
#[test]
fn preserves_unquoted_whitespace() {
    assert_eq!(parse_csv(" x , y ").unwrap(), vec![vec![" x ", " y "]]);
}

/// RFC 4180 §2.5 — an empty quoted field is the empty string, not a literal quote.
/// (held-out: `""` and `a,"",c`; here `"",z`.)
#[test]
fn empty_quoted_field_is_empty_string() {
    assert_eq!(parse_csv("\"\",z").unwrap(), vec![vec!["", "z"]]);
}

/// RFC 4180 §2.1 — CRLF separates records, and the CR must not leak into the
/// field. (held-out: `a,b\r\nc,d`; here `p,q\r\nr,s`.)
#[test]
fn crlf_record_separator() {
    assert_eq!(
        parse_csv("p,q\r\nr,s").unwrap(),
        vec![vec!["p", "q"], vec!["r", "s"]]
    );
}

// --- mirrored skills that BITE: malformed quoting must be rejected ------------
// These are the rungs a single shot is most likely to miss, so they're where the
// climb's gradient lives. (held-out mirrors each with different inputs.)

/// RFC 4180 §2.5 — a double quote inside an UNQUOTED field is malformed and must
/// return `Err`, not be silently accepted. (held-out: `ab"c"d`; here `x"y"z`.)
#[test]
fn quote_in_unquoted_field_is_error() {
    assert!(parse_csv("x\"y\"z").is_err());
}

/// RFC 4180 §2.5 — a closed quoted field must be followed by a delimiter or line
/// break; trailing text after the closing quote is malformed. (held-out: `"a"b`;
/// here `"m"n`.)
#[test]
fn text_after_closing_quote_is_error() {
    assert!(parse_csv("\"m\"n").is_err());
}
