use clap::{ArgAction, Parser, Subcommand};

use crate::config::VERSION;

#[derive(Debug, clap::Args)]
pub struct CreateCli {
	/// Sandbox name (slug, no spaces).
	pub name: String,
	/// Main rw mount (host path).
	pub mount: Option<String>,
	/// Additional rw mount (host path).
	#[arg(long = "rw", value_name = "path", action = ArgAction::Append)]
	pub rw: Vec<String>,
	/// Additional ro mount (host path).
	#[arg(long = "ro", value_name = "path", action = ArgAction::Append)]
	pub ro: Vec<String>,
	/// Don't use a flat fs (flat fs is required for docker).
	#[arg(long)]
	pub no_flat: bool,
	/// Replace an existing sandbox with the same name.
	#[arg(long)]
	pub replace: bool,
	/// Additional sandbox config file (overrides defaults).
	#[arg(long, value_name = "file", action = ArgAction::Append)]
	pub conf: Vec<std::path::PathBuf>,
	/// Additional resource config file.
	#[arg(long = "resource-conf", value_name = "file", action = ArgAction::Append)]
	pub resource_conf: Vec<std::path::PathBuf>,
	/// Additional network config file.
	#[arg(long = "net-conf", value_name = "file", action = ArgAction::Append)]
	pub net_conf: Vec<std::path::PathBuf>,
	/// Additional mounts/patches config file (merged with built-in mounts).
	#[arg(long = "fs-conf", value_name = "file", action = ArgAction::Append)]
	pub fs_conf: Vec<std::path::PathBuf>,
	/// Flat root disk size (default: 32G, e.g. 32G).
	#[arg(long = "root-disk-size", value_name = "size")]
	pub root_disk_size: Option<String>,
	/// Guest tmpfs (/tmp) size (default: 2G, e.g. 4G).
	#[arg(long = "tmpfs-size", value_name = "size")]
	pub tmpfs_size: Option<String>,
	/// Number of virtual CPUs to allocate (default: from resources.yml).
	#[arg(long, value_name = "n")]
	pub cpus: Option<u8>,
	/// Burstable CPU ceiling (overrides cpus as the maximum, e.g. 32).
	#[arg(long = "max-cpus", value_name = "n")]
	pub max_cpus: Option<u8>,
	/// Amount of memory to allocate (default: from resources.yml, e.g. 8G).
	#[arg(long, value_name = "size")]
	pub memory: Option<String>,
	/// Burstable memory ceiling (overrides memory as the maximum, e.g. 32G).
	#[arg(long = "max-memory", value_name = "size")]
	pub max_memory: Option<String>,
	/// Guest-write quota for the rw workspace mounts (default: 32G, e.g. 32G).
	#[arg(long, value_name = "size")]
	pub quota: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ListCli {
	/// Show only sandbox names.
	#[arg(short = 'q', long)]
	pub quiet: bool,
}

#[derive(Debug, clap::Args)]
pub struct NetCli {
	/// Sandbox name.
	pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct StartCli {
	/// Sandbox name.
	pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct StopCli {
	/// Stop all svamm sandboxes.
	#[arg(short = 'a', long)]
	pub all: bool,
	/// Sandbox name.
	pub name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct RestartCli {
	/// Sandbox name.
	pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct RmCli {
	/// Sandbox name.
	pub name: String,
}

/// arguments for `svamm run`.
#[derive(Debug, clap::Args)]
pub struct RunCli {
	/// Sandbox name.
	pub name: String,
	/// Open an interactive terminal with the default shell.
	#[arg(short, long)]
	pub tty: bool,
	/// Run as root (shell or command).
	#[arg(short, long)]
	pub root: bool,
	/// Command words (trailing arguments, typically after `--`).
	pub command: Vec<String>,
}

/// `svamm image` - image tooling (fetch base, build custom layers, mirror).
#[derive(Debug, clap::Args)]
pub struct ImageCli {
	#[command(subcommand)]
	pub command: ImageCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum ImageCommand {
	/// Fetch the configured vm base image into the local docker daemon as
	/// `svamm:<tag>` (tag of the configured `images.vm`), so a custom
	/// Dockerfile can build FROM it.
	Pull(ImagePullCli),
	/// Copy an image into the local registry so sandboxes can pull it through
	/// the read-only proxy.
	Mirror(ImageMirrorCli),
	/// Scaffold a custom vm image project (a Dockerfile FROM the svamm base).
	New(ImageNewCli),
	/// Build a custom vm image layer and push it to the local registry.
	Build(ImageBuildCli),
	/// List images stored in the local registry.
	#[command(visible_alias = "ls")]
	List(ImageListCli),
	/// Remove an image from the local registry and the msb cache (skipped
	/// while any sandbox still references it).
	Rm(ImageRmCli),
}

#[derive(Debug, clap::Args)]
pub struct ImagePullCli {
	/// Image reference to fetch (default: configured `images.vm`).
	pub reference: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ImageMirrorCli {
	/// Image reference to mirror (default: configured `images.vm`).
	pub reference: Option<String>,
	/// Name to store in the local registry (`repo[:tag]`, default: derived
	/// from the source reference, or `svamm:<tag>` for the default base).
	#[arg(long = "as", value_name = "name")]
	pub as_name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ImageNewCli {
	/// Directory to scaffold the custom image project in.
	pub dir: std::path::PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct ImageListCli {}

#[derive(Debug, clap::Args)]
pub struct ImageRmCli {
	/// Image to remove (`repo:tag`, e.g. `svamm:old-1` or `acme/agent:1.2.3`).
	pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct ImageBuildCli {
	/// Directory containing the custom Dockerfile.
	pub dir: std::path::PathBuf,
	/// Name to store in the local registry (`repo:tag`, e.g. `acme/agent:latest`).
	#[arg(long, value_name = "repo:tag")]
	pub name: String,
	/// Rebuild layers without using the docker build cache.
	#[arg(long)]
	pub no_cache: bool,
}

/// arguments for `svamm completions`.
#[derive(Debug, clap::Args)]
pub struct CompletionCli {
	/// Shell to generate the completion script for.
	#[arg(value_enum)]
	pub shell: clap_complete::Shell,
	/// Binary name the script completes (default: svamm; use `svm` for the abbreviation).
	#[arg(long, default_value = "svamm")]
	pub name: String,
}

/// the svamm command line interface.
#[derive(Debug, Parser)]
#[command(
	name = "svamm",
	version = VERSION,
	about = "a sandboxing thing"
)]
pub struct Cli {
	#[command(subcommand)]
	pub command: Option<Command>,
}

/// svamm subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
	/// Sets up this toolchain.
	Setup,
	/// Creates a sandbox.
	Create(Box<CreateCli>),
	/// Lists sandboxes.
	#[command(visible_alias = "ls")]
	List(ListCli),
	/// Lists a sandbox's network policy.
	Net(NetCli),
	/// Starts a sandbox.
	Start(StartCli),
	/// Stops a sandbox (-a: all).
	Stop(StopCli),
	/// Restarts a sandbox.
	Restart(RestartCli),
	/// Stops and removes a sandbox.
	Rm(RmCli),
	/// Runs a command in a sandbox.
	Run(RunCli),
	/// Image tooling: fetch the base, build custom layers, mirror images.
	Image(ImageCli),
	/// Deletes svamm images no longer referenced by sandboxes.
	Prune,
	/// Generates a shell completion script.
	Completions(CompletionCli),
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn image_build_parses_no_cache_flag() {
		let cli = Cli::parse_from([
			"svamm",
			"image",
			"build",
			"dir",
			"--name",
			"acme/agent:latest",
			"--no-cache",
		]);
		match cli.command {
			Some(Command::Image(ImageCli {
				command: ImageCommand::Build(args),
			})) => {
				assert!(args.no_cache, "--no-cache must be accepted");
				assert_eq!(args.dir, std::path::PathBuf::from("dir"));
				assert_eq!(args.name, "acme/agent:latest");
			}
			_ => panic!("expected `image build`"),
		}
	}

	#[test]
	fn image_build_without_no_cache_is_default_false() {
		let cli = Cli::parse_from([
			"svamm",
			"image",
			"build",
			"dir",
			"--name",
			"acme/agent:latest",
		]);
		match cli.command {
			Some(Command::Image(ImageCli {
				command: ImageCommand::Build(args),
			})) => assert!(!args.no_cache),
			_ => panic!("expected `image build`"),
		}
	}

	#[test]
	fn image_ls_and_alias_parse() {
		for argv in [["svamm", "image", "list"], ["svamm", "image", "ls"]] {
			let cli = Cli::parse_from(argv);
			assert!(
				matches!(
					cli.command,
					Some(Command::Image(ImageCli {
						command: ImageCommand::List(_)
					}))
				),
				"`{}` must parse as image list",
				argv.join(" ")
			);
		}
	}

	#[test]
	fn image_rm_parses_repo_tag() {
		let cli = Cli::parse_from(["svamm", "image", "rm", "svamm:old-1"]);
		match cli.command {
			Some(Command::Image(ImageCli {
				command: ImageCommand::Rm(args),
			})) => assert_eq!(args.name, "svamm:old-1"),
			_ => panic!("expected `image rm`"),
		}
	}

	#[test]
	fn create_parses_tmpfs_size_flag() {
		let cli = Cli::parse_from(["svamm", "create", "alpha", "--tmpfs-size", "4G"]);
		match cli.command {
			Some(Command::Create(args)) => {
				assert_eq!(args.tmpfs_size.as_deref(), Some("4G"));
				assert!(args.root_disk_size.is_none());
			}
			_ => panic!("expected `create`"),
		}
	}

	#[test]
	fn create_parses_burstable_resource_flags() {
		let cli = Cli::parse_from([
			"svamm",
			"create",
			"alpha",
			"--cpus",
			"4",
			"--memory",
			"8G",
			"--max-cpus",
			"8",
			"--max-memory",
			"16G",
		]);
		match cli.command {
			Some(Command::Create(args)) => {
				assert_eq!(args.cpus, Some(4));
				assert_eq!(args.max_cpus, Some(8));
				assert_eq!(args.memory.as_deref(), Some("8G"));
				assert_eq!(args.max_memory.as_deref(), Some("16G"));
			}
			_ => panic!("expected `create`"),
		}
	}
}
