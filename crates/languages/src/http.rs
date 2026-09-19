pub(super) fn task_context() -> project::ContextProviderWithTasks {
    let templates = serde_json::from_str(include_str!("../../grammars/src/http/tasks.json"))
        .expect("built-in HTTP tasks must be valid");
    project::ContextProviderWithTasks::new(templates)
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_tasks_use_the_project_cli() {
        let tasks: task::TaskTemplates =
            serde_json::from_str(include_str!("../../grammars/src/http/tasks.json")).unwrap();
        assert_eq!(tasks.0.len(), 6);
        for template in &tasks.0 {
            assert_eq!(template.command, "httpyac");
            assert_eq!(template.cwd.as_deref(), Some("$ZED_WORKTREE_ROOT"));
            assert!(template.args.iter().any(|argument| argument == "$ZED_FILE"));
        }
        for tag in ["http-request", "http-request-named"] {
            assert_eq!(
                tasks
                    .0
                    .iter()
                    .filter(|task| task.tags.iter().any(|value| value == tag))
                    .count(),
                1,
            );
        }
    }
}
