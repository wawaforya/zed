#[derive(Clone, Default)]
pub struct Files {
    entries: std::collections::BTreeMap<std::path::PathBuf, Entry>,
}

#[derive(Clone)]
struct Entry {
    stamp: Option<(std::time::SystemTime, u64)>,
    environments: Result<std::collections::BTreeMap<String, Vec<String>>, String>,
}

impl Files {
    pub fn invalidate(&mut self, uri: &str) {
        if let Ok(uri) = url::Url::parse(uri)
            && let Ok(path) = uri.to_file_path()
        {
            self.entries.remove(&path);
        }
    }

    pub fn variables(
        &mut self,
        uri: &str,
        roots: &std::collections::BTreeSet<String>,
        selected: &[String],
    ) -> (std::collections::BTreeMap<String, String>, Vec<String>) {
        let mut variables = std::collections::BTreeMap::new();
        let mut errors = Vec::new();
        let Some(path) = url::Url::parse(uri)
            .ok()
            .and_then(|uri| uri.to_file_path().ok())
        else {
            return (variables, errors);
        };
        let Some(parent) = path.parent() else {
            return (variables, errors);
        };
        let root = roots
            .iter()
            .filter_map(|uri| url::Url::parse(uri).ok()?.to_file_path().ok())
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .unwrap_or_else(|| parent.to_owned());
        let mut directories = Vec::new();
        for directory in parent.ancestors() {
            directories.push(directory.to_owned());
            if directory == root {
                break;
            }
        }
        directories.retain(|directory| *directory != root.join(".zed"));
        directories.sort();
        let mut environments: std::collections::BTreeMap<
            String,
            std::collections::BTreeSet<String>,
        > = Default::default();
        for filename in ["http-client.env.json", "http-client.private.env.json"] {
            for path in directories
                .iter()
                .map(|directory| directory.join(filename))
                .chain(std::iter::once(root.join(".zed").join(filename)))
            {
                let stamp = std::fs::metadata(&path).ok().and_then(|metadata| {
                    metadata
                        .modified()
                        .ok()
                        .map(|modified| (modified, metadata.len()))
                });
                if !self
                    .entries
                    .get(&path)
                    .is_some_and(|entry| entry.stamp == stamp)
                {
                    let value = read(&path).map_err(|error| format!("{}: {error}", path.display()));
                    self.entries.insert(
                        path.clone(),
                        Entry {
                            stamp,
                            environments: value,
                        },
                    );
                }
                if let Some(entry) = self.entries.get(&path) {
                    match &entry.environments {
                        Ok(values) => {
                            for (name, keys) in values {
                                environments
                                    .entry(name.clone())
                                    .or_default()
                                    .extend(keys.iter().cloned());
                            }
                        }
                        Err(error) => errors.push(error.clone()),
                    }
                }
            }
        }
        for name in std::iter::once("$shared").chain(selected.iter().map(String::as_str)) {
            if let Some(keys) = environments.get(name) {
                for key in keys {
                    variables.insert(key.clone(), name.to_owned());
                }
            }
        }
        (variables, errors)
    }
}

fn read(path: &std::path::Path) -> anyhow::Result<std::collections::BTreeMap<String, Vec<String>>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
        "Environment file is not a regular file or exceeds 16 MiB"
    );
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Environment configuration must be a JSON object"))?;
    let mut result = std::collections::BTreeMap::new();
    for (name, variables) in object {
        let variables = variables
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Environment {name} must be an object"))?;
        // Only retain names. Private values must never enter LSP responses or diagnostics.
        result.insert(name.clone(), variables.keys().cloned().collect());
    }
    Ok(result)
}
