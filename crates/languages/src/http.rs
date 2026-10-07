pub(super) fn task_context() -> HttpTaskContext {
    let templates = serde_json::from_str(include_str!("../../grammars/src/http/tasks.json"))
        .expect("built-in HTTP tasks must be valid");
    HttpTaskContext {
        templates,
        executor_directory: Default::default(),
    }
}

pub(super) struct HttpTaskContext {
    templates: task::TaskTemplates,
    executor_directory: std::sync::Arc<std::sync::OnceLock<anyhow::Result<tempfile::TempDir>>>,
}

impl language::ContextProvider for HttpTaskContext {
    fn build_context(
        &self,
        _: &task::TaskVariables,
        _: language::ContextLocation<'_>,
        _: Option<collections::HashMap<String, String>>,
        _: std::sync::Arc<dyn language::LanguageToolchainStore>,
        cx: &mut gpui::App,
    ) -> gpui::Task<anyhow::Result<task::TaskVariables>> {
        let directory = self.executor_directory.clone();
        gpui::AppContext::background_spawn(cx, async move {
            let directory = directory
                .get_or_init(|| {
                    let directory = tempfile::Builder::new()
                        .prefix("zed-httpyac-executor-")
                        .rand_bytes(16)
                        .tempdir()?;
                    std::fs::write(
                        directory.path().join("executor.cjs"),
                        include_str!("http_executor.cjs"),
                    )?;
                    Ok(directory)
                })
                .as_ref()
                .map_err(|error| anyhow::anyhow!("Failed to prepare HTTP executor: {error:#}"))?;
            // Context construction runs on the project host, including remote projects.
            // Only materialize code here: loading httpyac and executing scripts requires a task.
            Ok(task::TaskVariables::from_iter([
                (
                    task::VariableName::Custom(std::borrow::Cow::Borrowed("HTTPYAC_EXECUTOR")),
                    directory
                        .path()
                        .join("executor.cjs")
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    task::VariableName::Custom(std::borrow::Cow::Borrowed("HTTPYAC_OWNER")),
                    std::process::id().to_string(),
                ),
            ]))
        })
    }

    fn associated_tasks(
        &self,
        _: Option<gpui::Entity<language::Buffer>>,
        _: &gpui::App,
    ) -> gpui::Task<Option<task::TaskTemplates>> {
        gpui::Task::ready(Some(self.templates.clone()))
    }
}

pub struct HttpYacLspAdapter {
    node: node_runtime::NodeRuntime,
    fs: std::sync::Arc<dyn project::Fs>,
}

impl HttpYacLspAdapter {
    pub fn new(node: node_runtime::NodeRuntime, fs: std::sync::Arc<dyn project::Fs>) -> Self {
        Self { node, fs }
    }

    async fn bundled_binary(
        &self,
        _: &std::path::Path,
        delegate: &dyn language::LspAdapterDelegate,
    ) -> anyhow::Result<lsp::LanguageServerBinary> {
        Ok(lsp::LanguageServerBinary {
            path: std::env::current_exe()?,
            arguments: vec!["--httpyac-language-server".into()],
            env: Some(delegate.shell_env().await),
        })
    }
}

impl language::LspInstaller for HttpYacLspAdapter {
    type BinaryVersion = &'static str;

    async fn fetch_latest_server_version(
        &self,
        _: &std::sync::Arc<dyn language::LspAdapterDelegate>,
        _: bool,
        _: &mut gpui::AsyncApp,
    ) -> anyhow::Result<Self::BinaryVersion> {
        Ok("built-in-rust")
    }

    fn fetch_server_binary(
        &self,
        _: Self::BinaryVersion,
        container_dir: std::path::PathBuf,
        delegate: &std::sync::Arc<dyn language::LspAdapterDelegate>,
    ) -> impl Send + std::future::Future<Output = anyhow::Result<lsp::LanguageServerBinary>> + use<>
    {
        let adapter = Self::new(self.node.clone(), self.fs.clone());
        let delegate = delegate.clone();
        async move {
            adapter
                .bundled_binary(&container_dir, delegate.as_ref())
                .await
        }
    }

    async fn cached_server_binary(
        &self,
        container_dir: std::path::PathBuf,
        delegate: &dyn language::LspAdapterDelegate,
    ) -> Option<lsp::LanguageServerBinary> {
        util::ResultExt::log_err(self.bundled_binary(&container_dir, delegate).await)
    }
}

#[async_trait::async_trait(?Send)]
impl language::LspAdapter for HttpYacLspAdapter {
    fn name(&self) -> language::LanguageServerName {
        language::LanguageServerName::new_static("httpyac-language-server")
    }

    fn language_ids(&self) -> collections::HashMap<language::LanguageName, String> {
        [(language::LanguageName::new_static("HTTP"), "http".into())]
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn request_tasks_use_the_persistent_executor() {
        let tasks: task::TaskTemplates =
            serde_json::from_str(include_str!("../../grammars/src/http/tasks.json")).unwrap();
        assert_eq!(tasks.0.len(), 8);
        assert_eq!(
            tasks.0.iter().filter(|task| {
                task.env.get("ZED_HTTPYAC_UI").map(String::as_str) == Some("1")
            }).count(),
            7,
        );
        for template in &tasks.0 {
            assert_eq!(template.command, "node");
            assert_eq!(template.cwd.as_deref(), Some("$ZED_WORKTREE_ROOT"));
            assert_eq!(template.args[0], "-e");
            assert!(template.args[1].contains("process.env.ZED_CUSTOM_HTTPYAC_EXECUTOR"));
            assert!(
                template
                    .args
                    .iter()
                    .any(|argument| argument == "send" || argument == "reset")
            );
            assert!(
                !template
                    .args
                    .iter()
                    .any(|argument| argument.contains("$ZED_FILE"))
            );
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
