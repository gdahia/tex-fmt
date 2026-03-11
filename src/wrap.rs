//! Utilities for wrapping long lines

use crate::args::Args;
use crate::comments::find_comment_index;
use crate::format::{Pattern, State};
use crate::logging::{record_line_log, Log};
use crate::regexes::VERBS;
use log::Level;
use log::LevelFilter;
use std::path::Path;

/// String slice to start wrapped text lines
pub const TEXT_LINE_START: &str = "";
/// String slice to start wrapped comment lines
pub const COMMENT_LINE_START: &str = "% ";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum WrapKind {
    Space,
    Clause,
    Sentence,
}

/// Check if a line needs wrapping
#[must_use]
pub fn needs_wrap(line: &str, indent_length: usize, args: &Args) -> bool {
    args.wrap && (line.chars().count() + indent_length > args.wraplen)
}

fn get_wrap_kind(c: char, args: &Args) -> Option<WrapKind> {
    if args.semanticwrap && matches!(c, '.' | '!' | '?') {
        Some(WrapKind::Sentence)
    } else if matches!(c, ',' | ';' | ':') && args.wrap_chars.contains(&c) {
        Some(WrapKind::Clause)
    } else if args.wrap_chars.contains(&c) {
        Some(WrapKind::Space)
    } else {
        None
    }
}

fn is_wrap_point(
    c: char,
    prev_c: Option<char>,
    inside_verb: bool,
    args: &Args,
) -> Option<WrapKind> {
    if inside_verb || (c == ' ' && prev_c == Some('\\')) {
        None
    } else {
        get_wrap_kind(c, args)
    }
}

fn get_verb_end(verb_byte_start: Option<usize>, line: &str) -> Option<usize> {
    verb_byte_start.map(|v| {
        line[v..]
            .match_indices(['|', '+'])
            .nth(1)
            .map(|(i, _)| i + v)
    })?
}

fn is_inside_verb(
    i_byte: usize,
    contains_verb: bool,
    verb_start: Option<usize>,
    verb_end: Option<usize>,
) -> bool {
    if contains_verb {
        (verb_start.unwrap() <= i_byte) && (i_byte <= verb_end.unwrap())
    } else {
        false
    }
}

fn get_boundary_end(
    i_byte: usize,
    c: char,
    line: &str,
    wrap_kind: WrapKind,
) -> Option<usize> {
    if wrap_kind == WrapKind::Space {
        return line[i_byte + c.len_utf8()..]
            .chars()
            .any(|next| !next.is_whitespace())
            .then_some(i_byte + c.len_utf8() - 1);
    }

    let mut boundary_end = i_byte + c.len_utf8() - 1;
    let mut seen_whitespace = false;
    let after = &line[boundary_end + 1..];

    for (offset, next) in after.char_indices() {
        if matches!(next, '"' | '\'' | ')' | ']' | '}') && !seen_whitespace {
            boundary_end = i_byte + c.len_utf8() + offset + next.len_utf8() - 1;
        } else if next.is_whitespace() {
            seen_whitespace = true;
        } else {
            return seen_whitespace.then_some(boundary_end);
        }
    }

    None
}

/// Find the best place to break a long line.
/// Provided as a *byte* index, not a *char* index.
fn find_wrap_point(
    line: &str,
    indent_length: usize,
    args: &Args,
    pattern: &Pattern,
) -> Option<usize> {
    let contains_verb =
        pattern.contains_verb && VERBS.iter().any(|x| line.contains(x));
    let verb_start: Option<usize> = contains_verb
        .then(|| VERBS.iter().filter_map(|&x| line.find(x)).min().unwrap());

    let verb_end = get_verb_end(verb_start, line);
    let mut after_non_percent = verb_start == Some(0);
    let wrap_boundary = args.wrapmin.saturating_sub(indent_length);
    let wrap_limit = args.wraplen.saturating_sub(indent_length);
    let mut sentence_wrap_point: Option<usize> = None;
    let mut clause_wrap_point: Option<usize> = None;
    let mut space_wrap_point: Option<usize> = None;
    let mut space_limit_wrap_point: Option<usize> = None;
    let mut fallback_wrap_point: Option<(usize, WrapKind)> = None;
    let mut prev_c: Option<char> = None;

    for (i_char, (i_byte, c)) in line.char_indices().enumerate() {
        // Special wrapping for lines containing \verb|...|
        let inside_verb =
            is_inside_verb(i_byte, contains_verb, verb_start, verb_end);
        if let Some(wrap_kind) = is_wrap_point(c, prev_c, inside_verb, args) {
            if after_non_percent {
                if let Some(wrap_byte) =
                    get_boundary_end(i_byte, c, line, wrap_kind)
                {
                    if i_char <= wrap_limit {
                        match wrap_kind {
                            WrapKind::Sentence => {
                                if sentence_wrap_point.is_none() {
                                    sentence_wrap_point = Some(wrap_byte);
                                }
                            }
                            WrapKind::Clause => {
                                clause_wrap_point = Some(wrap_byte)
                            }
                            WrapKind::Space => {
                                space_limit_wrap_point = Some(wrap_byte);
                                if i_char <= wrap_boundary {
                                    space_wrap_point = Some(wrap_byte);
                                }
                            }
                        }
                    }

                    fallback_wrap_point = Some(match fallback_wrap_point {
                        Some((current_byte, current_kind))
                            if current_kind >= wrap_kind =>
                        {
                            (current_byte, current_kind)
                        }
                        _ => (wrap_byte, wrap_kind),
                    });
                }
            }
        } else if c != '%' {
            after_non_percent = true;
        }
        prev_c = Some(c);
    }

    sentence_wrap_point
        .or(clause_wrap_point)
        .or(space_wrap_point)
        .or(space_limit_wrap_point)
        .or_else(|| fallback_wrap_point.map(|(wrap_byte, _)| wrap_byte))
}

/// Wrap a long line into a short prefix and a suffix
pub fn apply_wrap<'a>(
    line: &'a str,
    indent_length: usize,
    state: &State,
    file: &Path,
    args: &Args,
    logs: &mut Vec<Log>,
    pattern: &Pattern,
) -> Option<[&'a str; 3]> {
    if args.verbosity == LevelFilter::Trace {
        record_line_log(
            logs,
            Level::Trace,
            file,
            state.linum_new,
            state.linum_old,
            line,
            "Wrapping long line.",
        );
    }
    let wrap_point = find_wrap_point(line, indent_length, args, pattern);
    let comment_index = find_comment_index(line, pattern);

    match wrap_point {
        Some(p) if p <= args.wraplen => {}
        _ => {
            record_line_log(
                logs,
                Level::Warn,
                file,
                state.linum_new,
                state.linum_old,
                line,
                "Line cannot be wrapped.",
            );
        }
    }

    wrap_point.map(|p| {
        let this_line = &line[0..=p];
        let next_line_start = comment_index.map_or("", |c| {
            if p > c {
                COMMENT_LINE_START
            } else {
                TEXT_LINE_START
            }
        });
        let next_line = &line[p + 1..];
        [this_line, next_line_start, next_line]
    })
}
