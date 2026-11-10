//! Golden-path diffing: compare two sessions' tool-call sequences to spot
//! when an agent's behavior changed between runs. Alignment is the longest
//! common subsequence of tool names; whatever does not line up is the delta.

/// The diff between two sessions' tool-call paths.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionDiff {
    pub a_calls: usize,
    pub b_calls: usize,
    /// Calls present in both, in the same order (the LCS length).
    pub common: usize,
    /// 0-based step in each session where the alignment first breaks.
    pub first_divergence: Option<(usize, usize)>,
    /// (step, name) of calls only in session A, in A's order.
    pub only_in_a: Vec<(usize, String)>,
    /// (step, name) of calls only in session B, in B's order.
    pub only_in_b: Vec<(usize, String)>,
}

/// Diff two ordered tool-name sequences.
pub fn diff_tool_sequences(a: &[String], b: &[String]) -> SessionDiff {
    let n = a.len();
    let m = b.len();
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if a[i] == b[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let mut common = 0usize;
    let mut first_divergence = None;
    let mut only_in_a = Vec::new();
    let mut only_in_b = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            common += 1;
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            if first_divergence.is_none() {
                first_divergence = Some((i, j));
            }
            only_in_a.push((i, a[i].clone()));
            i += 1;
        } else {
            if first_divergence.is_none() {
                first_divergence = Some((i, j));
            }
            only_in_b.push((j, b[j].clone()));
            j += 1;
        }
    }
    while i < n {
        if first_divergence.is_none() {
            first_divergence = Some((i, j));
        }
        only_in_a.push((i, a[i].clone()));
        i += 1;
    }
    while j < m {
        if first_divergence.is_none() {
            first_divergence = Some((i, j));
        }
        only_in_b.push((j, b[j].clone()));
        j += 1;
    }
    SessionDiff {
        a_calls: n,
        b_calls: m,
        common,
        first_divergence,
        only_in_a,
        only_in_b,
    }
}
