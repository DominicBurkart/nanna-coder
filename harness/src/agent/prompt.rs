pub struct PromptParts<'a> {
    pub card_prompt: &'a str,
    pub capability_block: &'a str,
    pub top_level_listing: &'a [String],
    pub tools: &'a [(&'a str, &'a str)],
    pub guidance: &'a str,
    pub topology: &'a str,
}

pub fn assemble_system_prompt(parts: &PromptParts<'_>) -> String {
    let mut sections: Vec<String> = Vec::new();
    push_text(&mut sections, parts.card_prompt);
    push_text(&mut sections, parts.capability_block);

    if !parts.top_level_listing.is_empty() {
        let mut entries: Vec<&str> = parts.top_level_listing.iter().map(String::as_str).collect();
        entries.sort_unstable();
        entries.dedup();
        sections.push(format!(
            "All file paths are relative to the repository root. Do not prefix them with a directory name or an absolute path. Top level of the repository:\n{}",
            entries
                .iter()
                .map(|entry| format!("- {entry}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !parts.tools.is_empty() {
        let mut tools: Vec<(&str, &str)> = parts.tools.to_vec();
        tools.sort_unstable();
        sections.push(format!(
            "Tools:\n{}",
            tools
                .iter()
                .map(|(name, line)| format!("- {name}: {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    push_text(&mut sections, parts.guidance);
    push_text(&mut sections, parts.topology);
    sections.join("\n\n")
}

fn push_text(sections: &mut Vec<String>, text: &str) {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        sections.push(trimmed.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> Vec<String> {
        vec!["src/".to_string(), "Cargo.toml".to_string()]
    }

    fn parts<'a>(listing: &'a [String], tools: &'a [(&'a str, &'a str)]) -> PromptParts<'a> {
        PromptParts {
            card_prompt: "CARD-PROMPT",
            capability_block: "CAPABILITY-BLOCK",
            top_level_listing: listing,
            tools,
            guidance: "GUIDANCE-TEXT",
            topology: "TOPOLOGY-TEXT",
        }
    }

    #[test]
    fn sections_appear_in_the_documented_order() {
        let listing = listing();
        let tools = [("read_file", "Read a file."), ("finish", "End the task.")];
        let prompt = assemble_system_prompt(&parts(&listing, &tools));

        let positions: Vec<usize> = [
            "CARD-PROMPT",
            "CAPABILITY-BLOCK",
            "repository root",
            "Cargo.toml",
            "read_file: Read a file.",
            "GUIDANCE-TEXT",
            "TOPOLOGY-TEXT",
        ]
        .iter()
        .map(|needle| {
            prompt
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        })
        .collect();

        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn output_is_byte_stable_regardless_of_input_order() {
        let forward_listing = listing();
        let mut reversed_listing = listing();
        reversed_listing.reverse();
        let forward_tools = [("a", "first"), ("b", "second")];
        let reversed_tools = [("b", "second"), ("a", "first")];

        let first = assemble_system_prompt(&parts(&forward_listing, &forward_tools));
        let second = assemble_system_prompt(&parts(&reversed_listing, &reversed_tools));

        assert_eq!(first, second);
        assert!(first.find("a: first").unwrap() < first.find("b: second").unwrap());
    }

    #[test]
    fn empty_sections_are_omitted() {
        let prompt = assemble_system_prompt(&PromptParts {
            card_prompt: "ONLY-CARD",
            capability_block: "",
            top_level_listing: &[],
            tools: &[],
            guidance: "",
            topology: "",
        });

        assert!(prompt.starts_with("ONLY-CARD"));
        assert!(!prompt.contains("\n\n\n"));
        assert!(!prompt.trim_end().ends_with(':'));
    }

    #[test]
    fn the_path_rule_demands_repository_root_relative_paths() {
        let listing = listing();
        let prompt = assemble_system_prompt(&parts(&listing, &[]));

        assert!(prompt.contains("relative to the repository root"));
    }
}
