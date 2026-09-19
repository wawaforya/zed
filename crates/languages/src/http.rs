pub(super) fn task_context() -> project::ContextProviderWithTasks {
    let templates = serde_json::from_str(include_str!("../../grammars/src/http/tasks.json"))
        .expect("built-in HTTP tasks must be valid");
    project::ContextProviderWithTasks::new(templates)
}

const SERVER: &str = include_str!("../../../tooling/httpyac-language-server/bundle/server.cjs");
const SERVER_DIGEST: &str =
    include_str!("../../../tooling/httpyac-language-server/bundle/server.sha256");

pub struct HttpYacLspAdapter {
    node: node_runtime::NodeRuntime,
    fs: std::sync::Arc<dyn project::Fs>,
}

impl HttpYacLspAdapter {
    pub fn new(node: node_runtime::NodeRuntime, fs: std::sync::Arc<dyn project::Fs>) -> Self {
        Self { node, fs }
    }

    async fn install_bundle(
        fs: &dyn project::Fs,
        container_dir: &std::path::Path,
    ) -> anyhow::Result<std::path::PathBuf> {
        let path = container_dir.join(format!("server-{}.cjs", SERVER_DIGEST.trim()));
        if fs
            .load(&path)
            .await
            .is_ok_and(|contents| contents == SERVER)
        {
            return Ok(path);
        }
        fs.create_dir(container_dir).await?;
        fs.atomic_write(path.clone(), SERVER.to_owned()).await?;
        Ok(path)
    }

    async fn bundled_binary(
        &self,
        container_dir: &std::path::Path,
        delegate: &dyn language::LspAdapterDelegate,
    ) -> anyhow::Result<lsp::LanguageServerBinary> {
        let server = Self::install_bundle(self.fs.as_ref(), container_dir).await?;
        Ok(lsp::LanguageServerBinary {
            path: self.node.binary_path().await?,
            arguments: vec![server.into(), "--stdio".into()],
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
        Ok(SERVER_DIGEST.trim())
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
    #[gpui::test]
    async fn bundled_server_is_reusable_and_repairs_corruption(cx: &mut gpui::TestAppContext) {
        let fs = fs::FakeFs::new(cx.background_executor.clone());
        let directory = std::path::Path::new("/httpyac-server");
        let path = super::HttpYacLspAdapter::install_bundle(fs.as_ref(), directory)
            .await
            .unwrap();
        assert_eq!(
            project::Fs::load(fs.as_ref(), &path).await.unwrap(),
            super::SERVER
        );
        assert_eq!(
            super::HttpYacLspAdapter::install_bundle(fs.as_ref(), directory)
                .await
                .unwrap(),
            path,
        );
        project::Fs::atomic_write(fs.as_ref(), path.clone(), "corrupt".into())
            .await
            .unwrap();
        super::HttpYacLspAdapter::install_bundle(fs.as_ref(), directory)
            .await
            .unwrap();
        assert_eq!(
            project::Fs::load(fs.as_ref(), &path).await.unwrap(),
            super::SERVER
        );
    }

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
