use anyhow::Context as _;

use super::config;
use db::store::TransactionOptions;
use std::collections::VecDeque;

pub fn load_from<M: Send + Sync + 'static>(
    store: &mut db::store::fjall::Store<M>,
    load_from: Vec<config::LoadSpec>,
) -> anyhow::Result<()> {
    if load_from.is_empty() {
        return Ok(());
    }

    tracing::debug!("Loading");

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .context("Unable to start transaction")?;

    for spec in load_from {
        let config::LoadSpec {
            scope,
            prefix,
            path,
            max_depth,
        } = spec;

        let prefix = prefix.as_deref().unwrap_or("/");
        let path = std::path::Path::new(&path);

        assert!(prefix.ends_with('/'));
        assert!(prefix.starts_with('/'));

        let scope = scope
            .as_deref()
            .map(db::domain::Scope::parse)
            .unwrap_or_default();

        let mut files = vec![];

        visit(path, max_depth, |name, path| {
            files.push((name.into_owned(), path));
        });

        for (name, path) in files {
            let content = std::fs::read(&path)
                .with_context(|| format!("unable to read: {}", path.display()))?;
            let name = format!("{}{}", prefix, name);

            let blob_id = tx.store_blob(scope, &content)?;

            let path = scope.path(&name);

            tracing::debug!("Storing {} @ {}", name, path);

            tx.link_blob(path, blob_id)?;
        }
    }

    tx.commit().context("unable to commit tx")?;

    Ok(())
}

pub fn visit<F>(root: &std::path::Path, max_depth: Option<u32>, mut visitor: F)
where
    F: for<'a> FnMut(std::borrow::Cow<'a, str>, std::path::PathBuf),
{
    let max_depth = max_depth.unwrap_or(u32::MAX);

    let mut queue = VecDeque::new();
    queue.push_front((root.to_path_buf(), 0));

    while let Some((path, depth)) = queue.pop_back() {
        if path.is_dir() {
            if depth + 1 > max_depth {
                continue;
            }

            let it = std::fs::read_dir(&path)
                .unwrap_or_else(|_| panic!("Unable to read directory: {}", path.display()));

            for entry in it {
                let entry = entry.expect("Unable to read directory entry");
                queue.push_back((entry.path(), depth + 1));
            }
        } else {
            let name = if root == path {
                let name = root.file_name().expect("The root file should have a name");
                std::borrow::Cow::Borrowed(
                    name.to_str().expect("Unable to encode filename into utf-8"),
                )
            } else {
                let t = path.strip_prefix(root).expect("path is outside of root.");
                std::borrow::Cow::Owned(t.display().to_string())
            };

            visitor(name, path);
        }
    }
}
