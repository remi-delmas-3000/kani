// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! This file contains our own local version of
//! the `Span` to `CoverageRegion` conversion defined in
//! https://github.com/rust-lang/rust/tree/master/compiler/rustc_codegen_llvm/src/coverageinfo/mapgen/spans.rs

use rustc_span::Span;
use rustc_span::source_map::SourceMap;
use rustc_span::{BytePos, SourceFile};
use std::fmt::{self, Debug, Formatter};
use tracing::debug;

#[derive(Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRegion {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Debug for SourceRegion {
    fn fmt(&self, fmt: &mut Formatter<'_>) -> fmt::Result {
        let &Self { start_line, start_col, end_line, end_col } = self;
        write!(fmt, "{start_line}:{start_col} - {end_line}:{end_col}")
    }
}

fn ensure_non_empty_span(source_map: &SourceMap, span: Span) -> Option<Span> {
    if !span.is_empty() {
        return Some(span);
    }

    // The span is empty, so try to enlarge it to cover an adjacent '{' or '}'.
    source_map
        .span_to_source(span, |src, start, end| try {
            // Adjusting span endpoints by `BytePos(1)` is normally a bug,
            // but in this case we have specifically checked that the character
            // we're skipping over is one of two specific ASCII characters, so
            // adjusting by exactly 1 byte is correct.
            if src.as_bytes().get(end).copied() == Some(b'{') {
                Some(span.with_hi(span.hi() + BytePos(1)))
            } else if start > 0 && src.as_bytes()[start - 1] == b'}' {
                Some(span.with_lo(span.lo() - BytePos(1)))
            } else {
                None
            }
        })
        .ok()?
}

/// If `llvm-cov` sees a source region that is improperly ordered (end < start),
/// it will immediately exit with a fatal error. To prevent that from happening,
/// discard regions that are improperly ordered, or might be interpreted in a
/// way that makes them improperly ordered.
fn check_source_region(source_region: SourceRegion) -> Option<SourceRegion> {
    let SourceRegion { start_line, start_col, end_line, end_col } = source_region;
    // Line/column coordinates are supposed to be 1-based. If we ever emit
    // coordinates of 0, `llvm-cov` might misinterpret them.
    let all_nonzero = [start_line, start_col, end_line, end_col].into_iter().all(|x| x != 0);
    // Coverage mappings use the high bit of `end_col` to indicate that a
    // region is actually a "gap" region, so make sure it's unset.
    let end_col_has_high_bit_unset = (end_col & (1 << 31)) == 0;
    // If a region is improperly ordered (end < start), `llvm-cov` will exit
    // with a fatal error, which is inconvenient for users and hard to debug.
    let is_ordered = (start_line, start_col) <= (end_line, end_col);
    if all_nonzero && end_col_has_high_bit_unset && is_ordered {
        Some(source_region)
    } else {
        debug!(
            ?source_region,
            ?all_nonzero,
            ?end_col_has_high_bit_unset,
            ?is_ordered,
            "Skipping source region that would be misinterpreted or rejected by LLVM"
        );
        // If this happens in a debug build, ICE to make it easier to notice.
        debug_assert!(false, "Improper source region: {source_region:?}");
        None
    }
}

/// Converts the span into its start line and column, and end line and column.
///
/// Line numbers and column numbers are 1-based. Unlike most column numbers emitted by
/// the compiler, these column numbers are denoted in **bytes**, because that's what
/// LLVM's `llvm-cov` tool expects to see in coverage maps.
///
/// Returns `None` if the conversion failed for some reason. This shouldn't happen,
/// but it's hard to rule out entirely (especially in the presence of complex macros
/// or other expansions), and if it does happen then skipping a span or function is
/// better than an ICE or `llvm-cov` failure that the user might have no way to avoid.
pub(crate) fn make_source_region(
    source_map: &SourceMap,
    file: &SourceFile,
    span: Span,
) -> Option<SourceRegion> {
    let span = ensure_non_empty_span(source_map, span)?;
    let lo = span.lo();
    let hi = span.hi();
    // Column numbers need to be in bytes, so we can't use the more convenient
    // `SourceMap` methods for looking up file coordinates.
    let line_and_byte_column = |pos: BytePos| -> Option<(usize, usize)> {
        let rpos = file.relative_position(pos);
        let line_index = file.lookup_line(rpos)?;
        let line_start = file.lines()[line_index];
        // Line numbers and column numbers are 1-based, so add 1 to each.
        Some((line_index + 1, ((rpos - line_start).0 as usize) + 1))
    };
    let (mut start_line, start_col) = line_and_byte_column(lo)?;
    let (mut end_line, end_col) = line_and_byte_column(hi)?;
    // Apply an offset so that code in doctests has correct line numbers.
    // FIXME(#79417): Currently we have no way to offset doctest _columns_.
    start_line = source_map.doctest_offset_line(&file.name, start_line);
    end_line = source_map.doctest_offset_line(&file.name, end_line);
    check_source_region(SourceRegion {
        start_line: start_line as u32,
        start_col: start_col as u32,
        end_line: end_line as u32,
        end_col: end_col as u32,
    })
}
