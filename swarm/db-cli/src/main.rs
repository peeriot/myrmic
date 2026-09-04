use std::io::BufRead as _;
use std::path::PathBuf;

#[derive(argh::FromArgs)]
/// This simple cli tool can be used to execute some commands against a running swarm network.
///
/// Executing it will drop you into an interactive command-line, and you can use that to execute commands against the network.
/// Commands such as:
///     `help`
///     `begin`
///     `commit`
///     `key_put`
///     `key_get`
///     `blob_store`
///     `blob_link`
///     `path_resolve`
struct Args {
    #[argh(positional)]
    file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let Args { file } = argh::from_env();

    let mut ctx = db_cli::Context::new().await;

    if let Some(file) = file {
        println!("executing file: {}", file.display());

        let value = std::fs::read_to_string(&file)
            .unwrap_or_else(|_| panic!("unable to read: {}", file.display()));

        for line in value.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            ctx.execute_line(line).await;
        }
    }

    println!("# Start by typing: `help`");

    let mut input = std::io::stdin().lock();
    let mut buffer = String::new();

    loop {
        let tx = if let Some(tx) = ctx.tx_id {
            let tx_id = uuid::Uuid::from_u64_pair(tx.0, tx.1);
            let node_id = uhlc::ID::try_from(&tx.2).unwrap();
            format!("{} via {}\n", tx_id, node_id)
        } else {
            String::new()
        };

        println!(
            "\n{}/{}/{}\n{}>> ",
            ctx.scope.namespace, ctx.scope.database, ctx.scope.schema, tx
        );

        let count = input.read_line(&mut buffer).expect("Unable to read input");

        let line = &buffer[..count];
        let line = line.trim();

        if count == 0 {
            buffer.clear();
            continue;
        }
        if line == "exit" {
            break;
        }

        println!();

        ctx.execute_line(line).await;

        buffer.clear();
    }
}
