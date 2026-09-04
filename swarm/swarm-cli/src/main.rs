use crate::cmd::Cmd;

mod cmd;
mod spawn;

fn main() -> anyhow::Result<()> {
    // Cursed, but fuck it, it works.
    // The idea here is that we can support more commands, but only support Spawn.
    // (We previously supported a different command, before it was removed, so the support is still here for now)
    let cmd = match <cmd::Args as clap::Parser>::try_parse() {
        Ok(cmd) => cmd.cmd,
        Err(e) => {
            // Some sugar, most users will just want to give a set of config files, and have it just managed for them.
            // So if we fail to read the args properly, we just delegate to the Spawn subcommand, which is probably what they want.

            let spawn = <cmd::Spawn as clap::Parser>::try_parse();
            match spawn {
                Ok(spawn) => Cmd::Spawn(spawn),
                Err(_) => e.exit(),
            }
        }
    };

    match cmd {
        Cmd::Spawn(cmd) => spawn::handle(&cmd),
        Cmd::Show(cmd) => show(cmd),
        Cmd::Eval(cmd) => eval(cmd),
    }
}

fn show(cmd: cmd::Spawn) -> anyhow::Result<()> {
    let config = swarm::Swarm::from_path(cmd.config)?.into_config();
    let json = serde_json::to_string_pretty(&config).expect("Unable to represent as json");
    println!("{}", json);
    Ok(())
}

fn eval(cmd: cmd::Spawn) -> anyhow::Result<()> {
    let values = swarm::eval_input::<serde_json::Value>(cmd.config)?;
    let json = serde_json::to_string_pretty(&values).expect("Unable to represent as json");
    println!("{}", json);
    Ok(())
}
