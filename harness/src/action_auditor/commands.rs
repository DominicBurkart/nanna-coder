use serde_json::Value;

pub(super) fn inspect(tool: &str, args: &Value) -> Result<(), String> {
    content_inspection_not_implemented(tool, args)
}

fn content_inspection_not_implemented(_tool: &str, _args: &Value) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_accepts_everything_because_no_content_inspection_exists_yet() {
        let args = serde_json::json!({ "command": "gh pr merge 5" });
        assert_eq!(inspect("run_command", &args), Ok(()));
        assert_eq!(
            content_inspection_not_implemented("run_command", &args),
            Ok(())
        );
    }
}
