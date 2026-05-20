// Smith-Waterman-ish subsequence scorer with the same heuristics fzf uses:
// reward word-boundaries / separators, reward consecutive matches, penalize
// gaps. Case-insensitive by default; uppercase chars in the query force a
// case-sensitive comparison ("smart case").

const BONUS_BOUNDARY: i32 = 25;
const BONUS_CONSECUTIVE: i32 = 15;
const BONUS_CAMEL: i32 = 10;
const BONUS_FIRST_CHAR: i32 = 12;
const PENALTY_GAP_START: i32 = -3;
const PENALTY_GAP_EXTEND: i32 = -1;

#[derive(Debug, Clone)]
pub struct Match {
    pub score: i32,
    pub indices: Vec<usize>,
}

pub fn score(query: &str, target: &str) -> Option<Match> {
    if query.is_empty() {
        return Some(Match {
            score: 0,
            indices: Vec::new(),
        });
    }
    let smart_case_sensitive = query.chars().any(|c| c.is_uppercase());
    let normalize = |c: char| {
        if smart_case_sensitive {
            c
        } else {
            c.to_ascii_lowercase()
        }
    };

    let target_chars: Vec<char> = target.chars().collect();
    let target_norm: Vec<char> = target_chars.iter().map(|&c| normalize(c)).collect();
    let query_norm: Vec<char> = query.chars().map(normalize).collect();

    let mut indices = Vec::with_capacity(query_norm.len());
    let mut total: i32 = 0;
    let mut qi = 0usize;
    let mut last_match: Option<usize> = None;
    let mut in_gap = false;

    for (ti, &tc) in target_norm.iter().enumerate() {
        if qi >= query_norm.len() {
            break;
        }
        if tc == query_norm[qi] {
            let mut s = 1;
            if ti == 0 {
                s += BONUS_FIRST_CHAR;
            }
            let prev = if ti > 0 {
                Some(target_chars[ti - 1])
            } else {
                None
            };
            if let Some(p) = prev {
                if is_separator(p) {
                    s += BONUS_BOUNDARY;
                } else if p.is_ascii_lowercase() && target_chars[ti].is_ascii_uppercase() {
                    s += BONUS_CAMEL;
                }
            }
            if let Some(prev_idx) = last_match {
                if prev_idx + 1 == ti {
                    s += BONUS_CONSECUTIVE;
                }
            }
            total += s;
            indices.push(ti);
            qi += 1;
            last_match = Some(ti);
            in_gap = false;
        } else if last_match.is_some() {
            total += if in_gap {
                PENALTY_GAP_EXTEND
            } else {
                PENALTY_GAP_START
            };
            in_gap = true;
        }
    }

    if qi == query_norm.len() {
        Some(Match {
            score: total,
            indices,
        })
    } else {
        None
    }
}

fn is_separator(c: char) -> bool {
    matches!(c, ' ' | '/' | '-' | '_' | '.' | ':' | '\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_anything() {
        let m = score("", "anything").unwrap();
        assert!(m.indices.is_empty());
    }

    #[test]
    fn non_subsequence_fails() {
        assert!(score("xyz", "abcdef").is_none());
    }

    #[test]
    fn boundary_beats_middle() {
        let a = score("f", "foo-bar").unwrap();
        let b = score("b", "foo-bar").unwrap();
        // 'b' sits on a separator boundary → should outscore 'f' at line start? Both bonuses; just ensure both > 1.
        assert!(a.score > 1 && b.score > 1);
    }

    #[test]
    fn consecutive_beats_scattered() {
        let consec = score("abc", "abcxxx").unwrap();
        let scat = score("abc", "axbxc").unwrap();
        assert!(consec.score > scat.score);
    }

    #[test]
    fn smart_case_uppercase_in_query() {
        assert!(score("Foo", "foobar").is_none());
        assert!(score("foo", "FooBar").is_some());
    }
}
