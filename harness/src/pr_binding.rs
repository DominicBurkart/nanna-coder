use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const MAX_PART: usize = 100;

fn plain_part(part: &str) -> bool {
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
    let named = !part.is_empty() && part.len() <= MAX_PART && part != "." && part != "..";
    named && part.bytes().all(allowed)
}

#[doc = r#"A repository name reduced to the form two names are compared in: `owner/name`, both parts plain ASCII letters, digits, `-`, `_` or `.`, lower-cased. Anything else (a non-ASCII lookalike, a trailing slash, extra path parts, an empty part) is `None`, so it never equals anything.

```
use harness::pr_binding::normalised_repo;

assert_eq!(normalised_repo("Example/Repo").as_deref(), Some("example/repo"));
assert_eq!(normalised_repo("example/repo."), Some("example/repo.".to_string()));
assert_ne!(normalised_repo("example/repo."), normalised_repo("example/repo"));
assert_eq!(normalised_repo("exampl\u{435}/repo"), None);
assert_eq!(normalised_repo("example/repo/extra"), None);
assert_eq!(normalised_repo("example"), None);
```
"#]
pub fn normalised_repo(repo: &str) -> Option<String> {
    let (owner, name) = repo.split_once('/')?;
    let valid = plain_part(owner) && plain_part(name);
    valid.then(|| {
        format!(
            "{}/{}",
            owner.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        )
    })
}

#[doc = r#"Whether two repository names are the same repository after [`normalised_repo`]; a name that cannot be normalised is never the same as anything, itself included.

```
use harness::pr_binding::same_repo;

assert!(same_repo("Example/Repo", "example/repo"));
assert!(!same_repo("example/repo", "example/repo."));
assert!(!same_repo("exampl\u{435}/repo", "exampl\u{435}/repo"));
```
"#]
pub fn same_repo(left: &str, right: &str) -> bool {
    match (normalised_repo(left), normalised_repo(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[doc = r#"The pull request a task opened: its repository (normalised `owner/name`, resolved from `origin` by the harness), its number (from the host's response when it was created), the branch it was opened from and the commit that branch pointed at, as `git rev-parse HEAD` reported it. The fields are private and there is no way to build one from data or from tool arguments.

```compile_fail,E0451
use harness::pr_binding::PrBinding;

let _ = PrBinding { repo: "o/n".into(), number: 1, head_branch: "b".into(), head_sha: "0".repeat(40) };
```

```compile_fail,E0277
use harness::pr_binding::PrBinding;

let _ = serde_json::from_str::<PrBinding>("{}");
```
"#]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBinding {
    repo: String,
    number: u64,
    head_branch: String,
    head_sha: String,
}

impl PrBinding {
    pub(crate) fn new(repo: &str, number: u64, head_branch: &str, head_sha: &str) -> Option<Self> {
        let repo = normalised_repo(repo)?;
        let sha = head_sha.len() == 40 && head_sha.bytes().all(|b| b.is_ascii_hexdigit());
        let named = number > 0 && !head_branch.is_empty();
        (sha && named).then(|| Self {
            repo,
            number,
            head_branch: head_branch.to_string(),
            head_sha: head_sha.to_ascii_lowercase(),
        })
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn head_branch(&self) -> &str {
        &self.head_branch
    }

    pub fn head_sha(&self) -> &str {
        &self.head_sha
    }
}

#[doc = r#"A task's one-shot pull request binding, shared by the tools of one tool registry: the first binding wins and nothing can replace or clear it. Only the harness's own `github_pr_open` can bind (after the host created the pull request); the cell's `bind` is crate-private, so no tool argument, queue field or other crate can set it.

```
use harness::pr_binding::PrBindingCell;

let cell = PrBindingCell::new();
assert!(cell.get().is_none());
assert_eq!(cell.number_for("example/repo"), None);
```

```compile_fail,E0624
use harness::pr_binding::PrBindingCell;

let cell = PrBindingCell::new();
cell.bind(todo!());
```
"#]
#[derive(Debug, Clone, Default)]
pub struct PrBindingCell(Arc<Mutex<Option<Arc<PrBinding>>>>);

impl PrBindingCell {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self) -> MutexGuard<'_, Option<Arc<PrBinding>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn bind(&self, binding: PrBinding) -> bool {
        let mut slot = self.slot();
        if slot.is_some() {
            return false;
        }
        *slot = Some(Arc::new(binding));
        true
    }

    pub fn get(&self) -> Option<Arc<PrBinding>> {
        self.slot().clone()
    }

    pub fn number_for(&self, repo: &str) -> Option<u64> {
        let bound = self.get()?;
        same_repo(bound.repo(), repo).then_some(bound.number())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn binding(repo: &str, number: u64) -> PrBinding {
        PrBinding::new(repo, number, "feat/x", SHA).unwrap()
    }

    #[test]
    fn a_binding_normalises_its_repository_and_carries_its_parts() {
        let bound = binding("Owner/Name", 7);
        assert_eq!(bound.repo(), "owner/name");
        assert_eq!(bound.number(), 7);
        assert_eq!(bound.head_branch(), "feat/x");
        assert_eq!(bound.head_sha(), SHA);
        let upper = PrBinding::new("o/n", 1, "b", &SHA.to_ascii_uppercase()).unwrap();
        assert_eq!(upper.head_sha(), SHA);
    }

    #[test]
    fn a_binding_needs_a_plain_repository_a_number_a_branch_and_a_full_sha() {
        assert!(PrBinding::new("o/n", 0, "b", SHA).is_none());
        assert!(PrBinding::new("o/n", 1, "", SHA).is_none());
        assert!(PrBinding::new("o/n", 1, "b", &SHA[..39]).is_none());
        assert!(PrBinding::new("o/n", 1, "b", &"g".repeat(40)).is_none());
        assert!(PrBinding::new("o", 1, "b", SHA).is_none());
        assert!(PrBinding::new("exampl\u{435}/n", 1, "b", SHA).is_none());
    }

    #[test]
    fn repository_names_with_odd_parts_never_normalise() {
        for odd in [
            "",
            "/",
            "o/",
            "/n",
            "o/n/x",
            "./n",
            "o/..",
            "o/n\n",
            "o /n",
            "o/n\u{200b}",
            "o/n:x",
        ] {
            assert_eq!(normalised_repo(odd), None, "{odd:?}");
        }
        assert_eq!(normalised_repo(&format!("{}/n", "a".repeat(101))), None);
        assert!(normalised_repo(&format!("{}/n", "a".repeat(100))).is_some());
        assert_eq!(normalised_repo("o/n").as_deref(), Some("o/n"));
        assert_eq!(
            normalised_repo("a-b_c.d/e-f_g.h").as_deref(),
            Some("a-b_c.d/e-f_g.h")
        );
    }

    #[test]
    fn two_names_match_only_when_they_are_the_same_plain_repository() {
        assert!(same_repo("O/N", "o/n"));
        assert!(!same_repo("o/n", "o/m"));
        assert!(!same_repo("o/n", "p/n"));
        assert!(!same_repo("o/n.", "o/n"));
        assert!(!same_repo("o/n", "o/n/"));
        assert!(!same_repo("o/\u{43d}", "o/\u{43d}"));
        assert!(!same_repo("", ""));
    }

    #[test]
    fn the_first_binding_wins_and_a_clone_shares_the_cell() {
        let cell = PrBindingCell::new();
        let shared = cell.clone();
        assert!(cell.get().is_none());
        assert!(cell.bind(binding("o/n", 7)));
        assert!(!shared.bind(binding("o/n", 8)));
        assert!(!cell.bind(binding("o/other", 9)));
        assert_eq!(shared.get().unwrap().number(), 7);
        assert_eq!(cell.get().unwrap().repo(), "o/n");
    }

    #[test]
    fn a_number_is_offered_only_for_the_bound_repository() {
        let cell = PrBindingCell::new();
        assert_eq!(cell.number_for("o/n"), None);
        cell.bind(binding("o/n", 7));
        assert_eq!(cell.number_for("O/N"), Some(7));
        assert_eq!(cell.number_for("o/m"), None);
        assert_eq!(cell.number_for("o/n."), None);
        assert_eq!(cell.number_for("github.com/o/n"), None);
        assert_eq!(cell.number_for(""), None);
    }
}
