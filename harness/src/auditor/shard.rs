use super::SpawnVerdict;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const SHARD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardSpec {
    pub index: usize,
    pub count: usize,
}

impl ShardSpec {
    pub fn whole() -> Self {
        Self { index: 1, count: 1 }
    }

    pub fn calls(&self, total: usize) -> Vec<usize> {
        let (offset, step) = (self.index - 1, self.count);
        (offset..total).step_by(step).collect()
    }
}

impl fmt::Display for ShardSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.index, self.count)
    }
}

impl FromStr for ShardSpec {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let bad = || format!("shard `{text}` must look like i/k with 1 <= i <= k");
        let (index, count) = text.split_once('/').ok_or_else(bad)?;
        let index: usize = index.trim().parse().map_err(|_| bad())?;
        let count: usize = count.trim().parse().map_err(|_| bad())?;
        let valid = index >= 1 && index <= count;
        valid.then_some(Self { index, count }).ok_or_else(bad)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    ModelOnly,
    Composed,
}

pub fn total_calls(cases: usize, runs: usize) -> usize {
    cases * runs + cases
}

pub fn call_at(call: usize, cases: usize, runs: usize) -> (Phase, usize, usize) {
    let model_only = cases * runs;
    if call < model_only {
        (Phase::ModelOnly, call / runs, call % runs)
    } else {
        (Phase::Composed, call - model_only, 0)
    }
}

pub fn call_index(phase: Phase, case: usize, run: usize, cases: usize, runs: usize) -> usize {
    match phase {
        Phase::ModelOnly => case * runs + run,
        Phase::Composed => cases * runs + case,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunIdentity {
    pub schema_version: u32,
    pub model: String,
    pub model_digest: String,
    pub temperature: f32,
    pub timeout_secs: u64,
    pub runs: usize,
    pub prompt_hash: String,
    pub repo_sha: String,
    pub shard_count: usize,
    pub case_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallOutcome {
    pub phase: Phase,
    pub case_id: String,
    pub run: usize,
    pub verdict: Option<SpawnVerdict>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardResult {
    pub identity: RunIdentity,
    pub shard_index: usize,
    pub calls: Vec<CallOutcome>,
}

impl ShardResult {
    pub fn file_name(&self) -> String {
        let (index, count) = (self.shard_index, self.identity.shard_count);
        format!("shard-{index}-of-{count}.json")
    }
}

pub fn differing_field(left: &RunIdentity, right: &RunIdentity) -> String {
    let left = serde_json::to_value(left).unwrap_or_default();
    let right = serde_json::to_value(right).unwrap_or_default();
    let fields = left.as_object().cloned().unwrap_or_default();
    let differs = |key: &&String| left.get(key.as_str()) != right.get(key.as_str());
    let found = fields.keys().find(differs);
    found.cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_shard_spec_parses_i_over_k_and_rejects_everything_else() {
        let spec: ShardSpec = "3/8".parse().unwrap();
        assert_eq!((spec.index, spec.count), (3, 8));
        assert_eq!(spec.to_string(), "3/8");
        assert_eq!(" 1 / 1 ".parse::<ShardSpec>().unwrap(), ShardSpec::whole());
        for bad in ["", "3", "0/3", "4/3", "1/0", "a/b", "1/2/3", "-1/3", "1/-3"] {
            assert!(bad.parse::<ShardSpec>().is_err(), "{bad}");
        }
    }

    #[test]
    fn slicing_is_round_robin_over_the_call_index() {
        let spec: ShardSpec = "2/3".parse().unwrap();
        assert_eq!(spec.calls(8), vec![1, 4, 7]);
        assert_eq!(ShardSpec::whole().calls(4), vec![0, 1, 2, 3]);
        assert!(ShardSpec { index: 5, count: 5 }.calls(3).is_empty());
    }

    #[test]
    fn calls_map_to_phase_case_and_run_and_back() {
        let (cases, runs) = (4, 3);
        assert_eq!(total_calls(cases, runs), 16);
        assert_eq!(call_at(0, cases, runs), (Phase::ModelOnly, 0, 0));
        assert_eq!(call_at(7, cases, runs), (Phase::ModelOnly, 2, 1));
        assert_eq!(call_at(11, cases, runs), (Phase::ModelOnly, 3, 2));
        assert_eq!(call_at(12, cases, runs), (Phase::Composed, 0, 0));
        assert_eq!(call_at(15, cases, runs), (Phase::Composed, 3, 0));
        for call in 0..total_calls(cases, runs) {
            let (phase, case, run) = call_at(call, cases, runs);
            assert_eq!(call_index(phase, case, run, cases, runs), call);
        }
    }

    proptest! {
        #[test]
        fn every_call_lands_in_exactly_one_shard(cases in 1usize..40, runs in 1usize..6, count in 1usize..70) {
            let total = total_calls(cases, runs);
            let mut seen = vec![0usize; total];
            for index in 1..=count {
                for call in (ShardSpec { index, count }).calls(total) {
                    seen[call] += 1;
                }
            }
            prop_assert!(seen.iter().all(|n| *n == 1));
        }

        #[test]
        fn every_case_run_pair_is_covered_exactly_once(cases in 1usize..30, runs in 1usize..5, count in 1usize..40) {
            let mut pairs = std::collections::HashMap::new();
            for index in 1..=count {
                for call in (ShardSpec { index, count }).calls(total_calls(cases, runs)) {
                    *pairs.entry(call_at(call, cases, runs)).or_insert(0usize) += 1;
                }
            }
            prop_assert_eq!(pairs.len(), total_calls(cases, runs));
            prop_assert!(pairs.values().all(|n| *n == 1));
        }

        #[test]
        fn shards_are_balanced_to_within_one_call(total in 0usize..400, count in 1usize..50) {
            let sizes: Vec<usize> = (1..=count).map(|index| (ShardSpec { index, count }).calls(total).len()).collect();
            let (low, high) = (sizes.iter().min().unwrap(), sizes.iter().max().unwrap());
            prop_assert!(high - low <= 1);
        }
    }

    #[test]
    fn the_differing_field_names_the_first_changed_identity_field() {
        let base = RunIdentity {
            schema_version: SHARD_SCHEMA_VERSION,
            model: "m".to_string(),
            model_digest: "d".to_string(),
            temperature: 0.0,
            timeout_secs: 600,
            runs: 3,
            prompt_hash: "p".to_string(),
            repo_sha: "s".to_string(),
            shard_count: 2,
            case_ids: vec!["a".to_string()],
        };
        let other = RunIdentity {
            repo_sha: "t".to_string(),
            ..base.clone()
        };
        assert_eq!(differing_field(&base, &other), "repo_sha");
        assert_eq!(differing_field(&base, &base), "");
        let result = ShardResult {
            identity: base,
            shard_index: 2,
            calls: vec![],
        };
        assert_eq!(result.file_name(), "shard-2-of-2.json");
    }
}
