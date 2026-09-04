use std::path::{Path, PathBuf};

#[derive(rust_embed::RustEmbed)]
#[folder = "lib/"]
struct ApiFiles;

pub fn eval_str<T, S>(content: S) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
    S: AsRef<str>,
{
    let state = state();
    let content = content.as_ref();

    eval_as::<T>(&state, vec![], &Input::Content(content))
}

pub fn eval_file<T, P>(path: P) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
    P: AsRef<Path>,
{
    let state = state();
    let path = path.as_ref();

    if !path.is_file() || path.extension().is_none_or(|ext| ext != "jsonnet") {
        anyhow::bail!("expected a jsonnet file.")
    }

    let folders = path
        .parent()
        .map(|path| vec![path.to_path_buf()])
        .unwrap_or_default();

    eval_as::<T>(&state, folders, &Input::Path(path))
}

enum Input<'a> {
    Content(&'a str),
    Path(&'a Path),
}

fn eval_as<T>(
    state: &jrsonnet_evaluator::State,
    folders: Vec<PathBuf>,
    input: &Input<'_>,
) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let text = eval(state, folders, input)?;

    let value: serde_json::Value = serde_json::from_str(&text).expect("Unable to parse json");
    // We do the round-trip in hopes that position info is easier to reason about.
    let text = serde_json::to_string_pretty(&value).expect("Unable to format json as text.");

    let item = serde_json::from_str(&text)?;
    Ok(item)
}

fn eval(
    state: &jrsonnet_evaluator::State,
    folders: Vec<PathBuf>,
    input: &Input<'_>,
) -> anyhow::Result<String> {
    let resolver = jrsonnet_importers::from_fn(move |from, path| {
        if !folders.is_empty()
            && let Some(source) = jrsonnet_importers::resolve_from(&folders, path)?
        {
            return Ok(Some(source));
        }

        if let Some(source) = jrsonnet_importers::resolve_embed::<ApiFiles>(from, path)? {
            return Ok(Some(source));
        }

        Ok(None)
    });

    state.set_import_resolver(resolver);

    let val = match input {
        Input::Path(path) => state.import(path),
        Input::Content(content) => state.evaluate_snippet("<chunk>", *content),
    }
    .map_err(|err| format_error(&err))?;

    let formatter = jrsonnet_evaluator::manifest::JsonFormat::minify(true);

    val.manifest(formatter).map_err(|err| format_error(&err))
}

fn format_error(error: &jrsonnet_evaluator::Error) -> anyhow::Error {
    use jrsonnet_evaluator::trace::TraceFormat;

    let format = jrsonnet_evaluator::trace::CompactFormat {
        resolver: jrsonnet_evaluator::trace::PathResolver::new_cwd_fallback(),
        max_trace: 20,
        padding: 4,
    };
    match format.format(error) {
        Ok(formatted) => anyhow::anyhow!("{}", formatted),
        Err(_) => anyhow::anyhow!("{}", error),
    }
}

fn state() -> jrsonnet_evaluator::State {
    let state = jrsonnet_evaluator::State::default();

    let ctx = jrsonnet_stdlib::ContextInitializer::new(
        state.clone(),
        jrsonnet_evaluator::trace::PathResolver::FileName,
    );
    state.set_context_initializer(ctx);

    state
}
