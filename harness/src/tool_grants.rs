use crate::identity::AgentIdentity;

pub const EDIT_FILE_TOOL: &str = "edit_file";
pub const WRITE_FILE_TOOL: &str = "write_file";

/// Whether `identity` grants `tool`. `edit_file` refines `write_file`, so it
/// is granted exactly when `write_file` is, whatever `scope.tools` says
/// about `edit_file` itself.
///
/// ```
/// use harness::identity::AgentIdentity;
/// use harness::tool_grants::identity_grants_tool;
///
/// let toml = r#"
/// [identity]
/// name = "editor"
/// description = "Edits."
/// loop = "inner"
/// model = "m"
/// system_prompt = { inline = "Edit." }
///
/// [scope]
/// repos = []
/// paths = ["**"]
/// max_effect = "workspace"
/// tools = ["read_file", "write_file"]
///
/// [limits]
/// max_iterations = 1
/// max_wall_clock_secs = 1
/// max_concurrent = 1
/// "#;
/// let identity = AgentIdentity::from_toml_str(toml, "editor.toml").unwrap();
/// assert!(identity_grants_tool(&identity, "edit_file"));
/// assert!(identity_grants_tool(&identity, "write_file"));
/// assert!(!identity_grants_tool(&identity, "search"));
/// ```
pub fn identity_grants_tool(identity: &AgentIdentity, tool: &str) -> bool {
    if tool == EDIT_FILE_TOOL {
        identity.allows_tool(WRITE_FILE_TOOL)
    } else {
        identity.allows_tool(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_tools(tools: &[&str]) -> AgentIdentity {
        let mut identity = crate::identity::example();
        identity.scope.tools = tools.iter().map(|t| t.parse().unwrap()).collect();
        identity
    }

    #[test]
    fn edit_file_follows_write_file_exactly() {
        assert!(identity_grants_tool(
            &with_tools(&["write_file"]),
            "edit_file"
        ));
        assert!(identity_grants_tool(&with_tools(&["*"]), "edit_file"));
        assert!(identity_grants_tool(&with_tools(&["write_*"]), "edit_file"));
    }

    #[test]
    fn edit_file_is_not_granted_by_its_own_name_or_without_write_file() {
        assert!(!identity_grants_tool(
            &with_tools(&["edit_file"]),
            "edit_file"
        ));
        assert!(!identity_grants_tool(
            &with_tools(&["read_file", "search"]),
            "edit_file"
        ));
        assert!(!identity_grants_tool(&with_tools(&[]), "edit_file"));
    }

    #[test]
    fn every_other_tool_is_judged_by_its_own_name() {
        let identity = with_tools(&["read_file"]);
        assert!(identity_grants_tool(&identity, "read_file"));
        assert!(!identity_grants_tool(&identity, "write_file"));
        assert!(!identity_grants_tool(&identity, "search"));
    }
}
