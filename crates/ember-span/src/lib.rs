#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }

    /// Combine two spans into one covering both — `self` provides the start,
    /// `other` provides the end.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start,
            end: other.end,
        }
    }
}

pub struct SourceMap {
    line_starts: Vec<u32>,
}

impl SourceMap {
    pub fn new(src: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        SourceMap { line_starts }
    }

    /// 1-based (line, column) for a byte offset, via binary search over
    /// precomputed line-start offsets.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            // safe: line_starts[0] == 0 and offset is a u32, so a search
            // miss can never land before index 0.
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        (line_idx as u32 + 1, offset - line_start + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_new_and_to() {
        let a = Span::new(0, 3);
        let b = Span::new(5, 8);
        assert_eq!(a.to(b), Span::new(0, 8));
    }

    #[test]
    fn span_is_8_bytes() {
        assert_eq!(std::mem::size_of::<Span>(), 8);
    }

    #[test]
    fn line_col_first_line() {
        let sm = SourceMap::new("abc\ndef\nghi");
        assert_eq!(sm.line_col(0), (1, 1));
        assert_eq!(sm.line_col(2), (1, 3));
    }

    #[test]
    fn line_col_second_line() {
        let sm = SourceMap::new("abc\ndef\nghi");
        assert_eq!(sm.line_col(4), (2, 1));
        assert_eq!(sm.line_col(6), (2, 3));
    }

    #[test]
    fn line_col_third_line() {
        let sm = SourceMap::new("abc\ndef\nghi");
        assert_eq!(sm.line_col(8), (3, 1));
    }
}
