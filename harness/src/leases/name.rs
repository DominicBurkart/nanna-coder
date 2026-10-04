use serde::{Deserialize, Serialize};
use std::fmt;

/// Category of a lease. The variant order is the global acquisition order:
/// `deploy < branch < sandbox < paths`. Every holder that takes several
/// leases takes them in this order (then lexically by repository and scope),
/// so two holders can never wait on each other in a cycle.
///
/// ```
/// use harness::leases::LeaseKind;
///
/// assert!(LeaseKind::Deploy < LeaseKind::Branch);
/// assert!(LeaseKind::Branch < LeaseKind::Sandbox);
/// assert!(LeaseKind::Sandbox < LeaseKind::Paths);
/// assert_eq!(LeaseKind::Deploy.name(), "deploy");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
    /// A rollout to one environment of a repository.
    Deploy,
    /// Pushes to one branch of a repository.
    Branch,
    /// The sandbox deployment of one pull request.
    Sandbox,
    /// Edits to a set of paths within a repository.
    Paths,
}

impl LeaseKind {
    /// Every kind, in acquisition order.
    pub const ALL: [LeaseKind; 4] = [
        LeaseKind::Deploy,
        LeaseKind::Branch,
        LeaseKind::Sandbox,
        LeaseKind::Paths,
    ];

    /// Prefix used in the string form of a lease name.
    pub const fn name(self) -> &'static str {
        match self {
            LeaseKind::Deploy => "deploy",
            LeaseKind::Branch => "branch",
            LeaseKind::Sandbox => "sandbox",
            LeaseKind::Paths => "paths",
        }
    }
}

impl fmt::Display for LeaseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Identity of a lease: `<kind>:<repo>:<scope>`.
///
/// Names are totally ordered by `(kind, repo, scope)`, which is the order in
/// which [`acquire_all`](super::acquire_all) takes them.
///
/// ```
/// use harness::leases::LeaseName;
///
/// assert_eq!(LeaseName::deploy("example/repo", "prod").to_string(), "deploy:example/repo:prod");
/// assert_eq!(LeaseName::branch("example/repo", "main").to_string(), "branch:example/repo:main");
/// assert_eq!(LeaseName::sandbox("example/repo", 42).to_string(), "sandbox:example/repo:42");
///
/// let paths = LeaseName::paths("example/repo", &["src/**", "Cargo.toml"]);
/// assert_eq!(paths, LeaseName::paths("example/repo", &["Cargo.toml", "src/**"]));
/// assert_eq!(paths.to_string(), "paths:example/repo:45a025fcf7240418");
///
/// let mut names = vec![paths.clone(), LeaseName::branch("example/repo", "main"), LeaseName::deploy("example/repo", "prod")];
/// names.sort();
/// assert_eq!(names[0].to_string(), "deploy:example/repo:prod");
/// assert_eq!(names[2], paths);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseName {
    kind: LeaseKind,
    repo: String,
    scope: String,
}

impl LeaseName {
    /// `deploy:<repo>:<env>`: a rollout to `env`.
    pub fn deploy(repo: impl Into<String>, env: impl Into<String>) -> Self {
        Self::new(LeaseKind::Deploy, repo, env)
    }

    /// `branch:<repo>:<branch>`: pushes to `branch`.
    pub fn branch(repo: impl Into<String>, branch: impl Into<String>) -> Self {
        Self::new(LeaseKind::Branch, repo, branch)
    }

    /// `sandbox:<repo>:<pr>`: the sandbox deployment of pull request `pr`.
    pub fn sandbox(repo: impl Into<String>, pr: u64) -> Self {
        Self::new(LeaseKind::Sandbox, repo, pr.to_string())
    }

    /// `paths:<repo>:<hash>` where `hash` is the FNV-1a digest of the sorted,
    /// deduplicated globs. The same set of globs in any order yields the same
    /// name.
    pub fn paths<S: AsRef<str>>(repo: impl Into<String>, globs: &[S]) -> Self {
        let mut sorted: Vec<&str> = globs.iter().map(AsRef::as_ref).collect();
        sorted.sort_unstable();
        sorted.dedup();
        Self::new(LeaseKind::Paths, repo, format!("{:016x}", fnv1a(&sorted)))
    }

    fn new(kind: LeaseKind, repo: impl Into<String>, scope: impl Into<String>) -> Self {
        Self {
            kind,
            repo: repo.into(),
            scope: scope.into(),
        }
    }

    /// Category of the lease.
    pub fn kind(&self) -> LeaseKind {
        self.kind
    }

    /// Repository the lease belongs to.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Environment, branch, pull request number or path-set hash.
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

impl fmt::Display for LeaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.kind, self.repo, self.scope)
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a over the globs separated by newlines. Stable across Rust
/// releases, unlike `DefaultHasher`, so persisted names keep meaning.
fn fnv1a(globs: &[&str]) -> u64 {
    let mut hash = FNV_OFFSET;
    for (i, glob) in globs.iter().enumerate() {
        if i > 0 {
            hash = (hash ^ u64::from(b'\n')).wrapping_mul(FNV_PRIME);
        }
        for byte in glob.bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::cmp::Ordering;

    #[test]
    fn standard_forms_match_the_issue() {
        assert_eq!(
            LeaseName::deploy("example/repo", "staging").to_string(),
            "deploy:example/repo:staging"
        );
        assert_eq!(
            LeaseName::branch("example/repo", "feat/x").to_string(),
            "branch:example/repo:feat/x"
        );
        assert_eq!(
            LeaseName::sandbox("example/repo", 7).to_string(),
            "sandbox:example/repo:7"
        );
        let name = LeaseName::paths("example/repo", &["a", "b"]);
        assert_eq!(name.kind(), LeaseKind::Paths);
        assert_eq!(name.repo(), "example/repo");
        assert_eq!(name.scope().len(), 16);
        assert_eq!(
            name.to_string(),
            format!("paths:example/repo:{}", name.scope())
        );
    }

    #[test]
    fn paths_hash_ignores_order_and_duplicates_but_not_content() {
        let a = LeaseName::paths("r", &["src/**", "Cargo.toml", "src/**"]);
        let b = LeaseName::paths("r", &["Cargo.toml", "src/**"]);
        let c = LeaseName::paths("r", &["Cargo.toml", "src/*"]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(
            LeaseName::paths("r", &["ab", "c"]),
            LeaseName::paths("r", &["a", "bc"])
        );
        let empty: [&str; 0] = [];
        assert_eq!(LeaseName::paths("r", &empty).scope(), "cbf29ce484222325");
    }

    #[test]
    fn kinds_sort_deploy_branch_sandbox_paths_then_lexically() {
        let mut names = [
            LeaseName::paths("a", &["x"]),
            LeaseName::sandbox("a", 1),
            LeaseName::branch("b", "main"),
            LeaseName::branch("a", "main"),
            LeaseName::branch("a", "dev"),
            LeaseName::deploy("z", "prod"),
            LeaseName::deploy("a", "prod"),
        ];
        names.sort();
        let rendered: Vec<String> = names.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendered,
            vec![
                "deploy:a:prod",
                "deploy:z:prod",
                "branch:a:dev",
                "branch:a:main",
                "branch:b:main",
                "sandbox:a:1",
                LeaseName::paths("a", &["x"]).to_string().as_str(),
            ]
        );
        assert_eq!(LeaseKind::ALL.to_vec(), {
            let mut kinds = LeaseKind::ALL.to_vec();
            kinds.sort();
            kinds
        });
        assert_eq!(LeaseKind::Sandbox.to_string(), "sandbox");
        let json = serde_json::to_string(&LeaseName::deploy("r", "e")).unwrap();
        assert_eq!(json, r#"{"kind":"deploy","repo":"r","scope":"e"}"#);
        let back: LeaseName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, LeaseName::deploy("r", "e"));
    }

    fn arb_name() -> impl Strategy<Value = LeaseName> {
        let repo = "[a-c]{1,2}";
        let scope = "[a-c]{1,2}";
        prop_oneof![
            (repo, scope).prop_map(|(r, s)| LeaseName::deploy(r, s)),
            (repo, scope).prop_map(|(r, s)| LeaseName::branch(r, s)),
            (repo, 0u64..3).prop_map(|(r, pr)| LeaseName::sandbox(r, pr)),
            (repo, proptest::collection::vec(scope, 0..3))
                .prop_map(|(r, globs)| LeaseName::paths(r, &globs)),
        ]
    }

    proptest! {
        #[test]
        fn order_is_total(a in arb_name(), b in arb_name(), c in arb_name()) {
            let ab = a.cmp(&b);
            prop_assert_eq!(b.cmp(&a), ab.reverse());
            prop_assert_eq!(ab == Ordering::Equal, a == b);
            if ab != Ordering::Greater && b.cmp(&c) != Ordering::Greater {
                prop_assert_ne!(a.cmp(&c), Ordering::Greater);
            }
            if a.kind() != b.kind() {
                prop_assert_eq!(ab, a.kind().cmp(&b.kind()));
            } else if a.repo() != b.repo() {
                prop_assert_eq!(ab, a.repo().cmp(b.repo()));
            } else {
                prop_assert_eq!(ab, a.scope().cmp(b.scope()));
            }
        }

        #[test]
        fn every_permutation_sorts_identically(names in proptest::collection::vec(arb_name(), 0..6), seed in any::<u64>()) {
            let mut sorted = names.clone();
            sorted.sort();
            let mut shuffled = names;
            let mut state = seed;
            for i in (1..shuffled.len()).rev() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let j = (state >> 33) as usize % (i + 1);
                shuffled.swap(i, j);
            }
            shuffled.sort();
            prop_assert_eq!(shuffled, sorted);
        }
    }
}
