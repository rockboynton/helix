//! Unicode-safe transaction construction for full-text changes and new peers.
use helix_core::{ChangeSet, Rope, Tendril, Transaction};

pub(super) fn transaction(before: &str, after: &str) -> Transaction {
    let mut input = imara_diff::InternedInput::<char>::default();
    input.update_before(before.chars());
    input.update_after(after.chars());
    let diff = imara_diff::Diff::compute(imara_diff::Algorithm::Myers, &input);
    let chars = after.chars().collect::<Vec<_>>();
    let rope = Rope::from_str(before);
    let changes = diff.hunks().map(|hunk| {
        let replacement: String = chars[hunk.after.start as usize..hunk.after.end as usize]
            .iter()
            .collect();
        (
            hunk.before.start as usize,
            hunk.before.end as usize,
            (!replacement.is_empty()).then(|| Tendril::from(replacement)),
        )
    });
    Transaction::from(ChangeSet::from_changes(&rope, changes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_single_edits_converge_exhaustively() {
        for base in ["", "abc", "a🦀é\nz"] {
            let n = base.chars().count();
            let rope = Rope::from_str(base);
            let mut edits = Vec::new();
            for start in 0..=n {
                for end in start..=n {
                    for replacement in ["", "x", "🦀é"] {
                        edits.push(ChangeSet::from_changes(
                            &rope,
                            [(
                                start,
                                end,
                                (!replacement.is_empty()).then(|| Tendril::from(replacement)),
                            )]
                            .into_iter(),
                        ));
                    }
                }
            }
            for a in &edits {
                for b in &edits {
                    let mut left = rope.clone();
                    let mut right = rope.clone();
                    assert!(a.apply(&mut left));
                    assert!(b.clone().map(a.clone()).apply(&mut left));
                    assert!(b.apply(&mut right));
                    assert!(a.clone().map_with_assoc(b.clone(), true).apply(&mut right));
                    assert_eq!(left, right, "base={base:?}, a={a:?}, b={b:?}");
                }
            }
        }
    }
}
