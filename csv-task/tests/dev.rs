//! Dev test suite — the VISIBLE feedback tests.
//!
//! These are the example tests the proposing agent is allowed to see and run
//! while it iterates. They cover the simplest, most common RFC 4180 shapes so
//! the agent has a foothold. Each test is tiny and independent so the score is
//! a clean `k/N` fraction rather than one all-or-nothing pass/fail.
//!
//! Reference: RFC 4180, "Common Format and MIME Type for CSV Files".

use csv_task::parse_csv;

/// RFC 4180 §2.4 — "Within the header and each record, there may be one or
/// more fields, separated by commas." A single record of plain unquoted fields
/// splits on commas into one row.
#[test]
fn simple_row() {
    assert_eq!(parse_csv("a,b,c").unwrap(), vec![vec!["a", "b", "c"]]);
}

/// RFC 4180 §2.1 — "Each record is located on a separate line." Two records
/// separated by a line break parse into two distinct rows.
#[test]
fn multiple_rows() {
    assert_eq!(
        parse_csv("a,b\nc,d").unwrap(),
        vec![vec!["a", "b"], vec!["c", "d"]]
    );
}

/// RFC 4180 §2.4 — commas are field delimiters, so two adjacent commas in
/// `a,,c` denote an empty field between `a` and `c`.
#[test]
fn empty_middle_field() {
    assert_eq!(parse_csv("a,,c").unwrap(), vec![vec!["a", "", "c"]]);
}

/// RFC 4180 §2.6 — "Fields containing line breaks (CRLF), double quotes, and
/// commas should be enclosed in double-quotes." A comma inside a quoted field
/// is literal data, not a field delimiter.
#[test]
fn quoted_field_with_comma() {
    assert_eq!(parse_csv("\"a,b\",c").unwrap(), vec![vec!["a,b", "c"]]);
}
