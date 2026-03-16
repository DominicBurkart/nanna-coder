pub fn fibonacci(n: usize) -> Vec<u64> {
    if n == 0 {
        return vec![];
    }
    let mut seq = vec![0u64, 1u64];
    while seq.len() < n {
        let len = seq.len();
        seq.push(seq[len - 1] + seq[len - 2]);
    }
    seq.truncate(n);
    seq
}
