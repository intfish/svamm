use clap::{CommandFactory, Parser};

use svamm::app;
use svamm::cli::{Cli, Command};
use svamm::config::Config;
use svamm::ui;

#[tokio::main]
async fn main() {
	let args: Vec<String> = std::env::args().skip(1).collect();

	let cli = Cli::try_parse_from(std::iter::once("svamm".to_string()).chain(args.iter().cloned()));
	let cli = match cli {
		Ok(cli) => cli,
		Err(err) => err.exit(),
	};

	// completions does not touch the config or the backend. handle it before any configuration loads.
	if let Some(Command::Completions(args)) = &cli.command {
		if let Err(err) = svamm::completions::run(args) {
			eprintln!("{} {err}", ui::style_error());
			std::process::exit(1);
		}
		return;
	}

	let config = Config::load().unwrap_or_else(|err| {
		eprintln!("{} {err}", ui::style_error());
		std::process::exit(1);
	});
	svamm::assets::ensure_default_configs(&config);

	let result = match cli.command {
		Some(Command::Setup) => app::setup(&config).await,
		Some(Command::Create(args)) => app::create(&config, &args).await,
		Some(Command::List(args)) => app::list(&config, &args).await,
		Some(Command::Net(args)) => app::net(&args).await,
		Some(Command::Mounts(args)) => app::mounts(&args).await,
		Some(Command::Start(args)) => app::start(&config, &args).await,
		Some(Command::Stop(args)) => app::stop(&args).await,
		Some(Command::Restart(args)) => app::restart(&config, &args).await,
		Some(Command::Rm(args)) => app::rm(&args).await,
		Some(Command::Run(args)) => app::run(&config, &args).await,
		Some(Command::Image(args)) => app::image(&config, &args).await,
		Some(Command::Prune) => app::prune(&config).await,
		Some(Command::Completions(_)) => unreachable!("handled before config load"),
		None => {
			Cli::command().print_help().ok();
			println!();
			return;
		}
	};

	if let Err(err) = result {
		eprintln!("{} {err}", ui::style_error());
		std::process::exit(1);
	}
}
