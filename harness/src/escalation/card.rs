use serde::{Deserialize, Serialize};

/// What a `needs-card` escalation asks the human to author: the identity
/// the auditor believes the task needs, in terms it could not find in the
/// catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRequest {
    /// Proposed identity name.
    pub name: String,
    /// `inner`, `middle` or `outer`.
    pub dev_loop: String,
    /// Effect ceiling the task needs (`none`, `local`, `repository`,
    /// `sandbox` or `production`).
    pub max_effect: String,
    /// Tools the task asked for.
    pub tools: Vec<String>,
    /// Why no existing card fits.
    pub reason: String,
}

#[derive(Serialize)]
struct Skeleton<'a> {
    identity: Identity<'a>,
    scope: Scope<'a>,
    limits: Limits,
}

#[derive(Serialize)]
struct Identity<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(rename = "loop")]
    dev_loop: &'a str,
    model: &'static str,
    system_prompt: InlinePrompt,
}

#[derive(Serialize)]
struct InlinePrompt {
    inline: &'static str,
}

#[derive(Serialize)]
struct Scope<'a> {
    repos: [&'static str; 1],
    paths: [&'static str; 1],
    max_effect: &'a str,
    tools: &'a [String],
}

#[derive(Serialize)]
struct Limits {
    max_iterations: u64,
    max_wall_clock_secs: u64,
    max_concurrent: u64,
}

/// Value written into every field the human must fill in: `identity.model`,
/// `identity.system_prompt`, `scope.repos[0]` and `scope.paths[0]`. The
/// identity loader rejects it as empty, so an unedited skeleton never loads.
pub const PLACEHOLDER: &str = "";

impl CardRequest {
    /// Render the identity TOML skeleton in the shape the identity loader
    /// expects: an `[identity]` table with `name`, `description`, `loop`, and
    /// [`PLACEHOLDER`] `model` and `system_prompt`; `[scope]` with
    /// [`PLACEHOLDER`] `repos` and `paths`, the requested `max_effect` and
    /// `tools`; and default `[limits]`. The loader rejects the skeleton until
    /// the human replaces every placeholder.
    ///
    /// ```
    /// use harness::escalation::CardRequest;
    ///
    /// let request = CardRequest {
    ///     name: "sandbox-qa".into(),
    ///     dev_loop: "middle".into(),
    ///     max_effect: "sandbox".into(),
    ///     tools: vec!["cargo_test".into()],
    ///     reason: "sandbox deploys need a card".into(),
    /// };
    /// let table: toml::Table = toml::from_str(&request.to_toml()).unwrap();
    /// assert_eq!(table["identity"]["name"].as_str(), Some("sandbox-qa"));
    /// assert_eq!(table["scope"]["max_effect"].as_str(), Some("sandbox"));
    /// assert_eq!(table["limits"]["max_iterations"].as_integer(), Some(100));
    /// ```
    pub fn to_toml(&self) -> String {
        let skeleton = Skeleton {
            identity: Identity {
                name: &self.name,
                description: &self.reason,
                dev_loop: &self.dev_loop,
                model: PLACEHOLDER,
                system_prompt: InlinePrompt {
                    inline: PLACEHOLDER,
                },
            },
            scope: Scope {
                repos: [PLACEHOLDER],
                paths: [PLACEHOLDER],
                max_effect: &self.max_effect,
                tools: &self.tools,
            },
            limits: Limits {
                max_iterations: 100,
                max_wall_clock_secs: 3600,
                max_concurrent: 1,
            },
        };
        toml::to_string_pretty(&skeleton).expect("identity skeleton serialises")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_has_every_section_in_order() {
        let request = CardRequest {
            name: "prod-fixer".to_string(),
            dev_loop: "outer".to_string(),
            max_effect: "production".to_string(),
            tools: vec![],
            reason: "quote \" and newline\nsurvive".to_string(),
        };
        let text = request.to_toml();
        assert!(text.starts_with("[identity]\nname = \"prod-fixer\"\n"));
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            table["identity"]["description"].as_str(),
            Some("quote \" and newline\nsurvive")
        );
        assert_eq!(table["identity"]["loop"].as_str(), Some("outer"));
        assert_eq!(table["identity"]["model"].as_str(), Some(PLACEHOLDER));
        assert_eq!(
            table["identity"]["system_prompt"]["inline"].as_str(),
            Some(PLACEHOLDER)
        );
        assert_eq!(table["scope"]["repos"][0].as_str(), Some(PLACEHOLDER));
        assert_eq!(table["scope"]["paths"][0].as_str(), Some(PLACEHOLDER));
        assert_eq!(table["scope"]["max_effect"].as_str(), Some("production"));
        assert_eq!(table["scope"]["tools"].as_array().map(Vec::len), Some(0));
        assert_eq!(
            table["limits"]["max_wall_clock_secs"].as_integer(),
            Some(3600)
        );
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<CardRequest>(json).unwrap(),
            request
        );
    }
}
