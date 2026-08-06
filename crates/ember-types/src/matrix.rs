use crate::pat::{CtorId, Pat};

pub type PatMatrix = Vec<Vec<Pat>>;

/// `S(c, matrix)`: keep rows whose first pattern is `c` itself or `Wild`
/// (matches anything), replacing that first column with `c`'s own
/// sub-patterns (or `arity` wildcards, for a `Wild` row).
pub fn specialize(ctor: &CtorId, arity: usize, matrix: &[Vec<Pat>]) -> PatMatrix {
    matrix
        .iter()
        .filter_map(|row| {
            let (first, rest) = row.split_first()?;
            match first {
                Pat::Wild => {
                    let mut new_row = vec![Pat::Wild; arity];
                    new_row.extend_from_slice(rest);
                    Some(new_row)
                }
                Pat::Ctor(c, args) if c == ctor => {
                    let mut new_row = args.clone();
                    new_row.extend_from_slice(rest);
                    Some(new_row)
                }
                Pat::Ctor(_, _) => None,
            }
        })
        .collect()
}

/// `D(matrix)`: keep rows whose first pattern is `Wild`, dropping that
/// column — the rows that say nothing about which constructor is used.
pub fn default_matrix(matrix: &[Vec<Pat>]) -> PatMatrix {
    matrix
        .iter()
        .filter_map(|row| {
            let (first, rest) = row.split_first()?;
            match first {
                Pat::Wild => Some(rest.to_vec()),
                Pat::Ctor(_, _) => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pat::{CtorId, Pat};

    #[test]
    fn specialize_keeps_matching_ctor_rows_and_expands_wild_rows() {
        let matrix = vec![
            vec![Pat::Ctor(CtorId::Int(0), vec![]), Pat::Wild],
            vec![Pat::Wild, Pat::Ctor(CtorId::Int(9), vec![])],
            vec![Pat::Ctor(CtorId::Int(1), vec![]), Pat::Wild],
        ];
        let out = specialize(&CtorId::Int(0), 0, &matrix);
        // Row 0 matches Int(0) exactly (arity 0, so no columns added) -> [Wild]
        // Row 1 is Wild-first -> expands to 0 wildcard columns + its rest -> [Int(9)]
        // Row 2 doesn't match Int(0) -> dropped
        assert_eq!(
            out,
            vec![vec![Pat::Wild], vec![Pat::Ctor(CtorId::Int(9), vec![])],]
        );
    }

    #[test]
    fn specialize_expands_a_wild_row_to_the_ctors_own_arity() {
        let matrix = vec![vec![Pat::Wild, Pat::Ctor(CtorId::Bool(true), vec![])]];
        let out = specialize(&CtorId::Cons, 2, &matrix);
        assert_eq!(
            out,
            vec![vec![
                Pat::Wild,
                Pat::Wild,
                Pat::Ctor(CtorId::Bool(true), vec![])
            ]]
        );
    }

    #[test]
    fn default_matrix_keeps_only_wild_first_rows_and_drops_the_column() {
        let matrix = vec![
            vec![Pat::Wild, Pat::Ctor(CtorId::Int(1), vec![])],
            vec![Pat::Ctor(CtorId::Int(0), vec![]), Pat::Wild],
            vec![Pat::Wild, Pat::Wild],
        ];
        let out = default_matrix(&matrix);
        assert_eq!(
            out,
            vec![vec![Pat::Ctor(CtorId::Int(1), vec![])], vec![Pat::Wild],]
        );
    }
}
