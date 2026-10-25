# svamm

a sandboxing thing

svamm is a wrapper around [microsandbox](https://github.com/superradcompany/microsandbox), mainly for ergonomics.
you can do most of this with just `msb` and bash scripts; it's how this happened.

_beware: this is not mature software and sharp edges exist_


## usage

```sh
# create a sandbox
svamm create foobar ~/path/to/project

# open a terminal
svamm run foobar -t

# open a root terminal
svamm run foobar -r

# run opencode
svamm run foobar

# can't undo this
svamm rm foobar

# svamm setup creates a symlink alias for even faster cli pizzazz
svm -> svamm
```

typical use:
- keep a `svamm.json` (often paired with an `opencode.json`) in your `~/path/to/project` dir
- depending on your situation you may even want to check them into git
- templating of these is left to the user as an exercise since every setup is different
- you can find a reference in `examples/`


## features

- quite hermetic by default
- per-project configs to override defaults
- arbitrary number of host mounts (ro, rw)
- quotas for root disk and host mounts
- network egress is deny all by default
- can publish ports to host
- base image comes with docker already set up
- sandbox container registry with host-side push only
- tooling to mirror private images to sandbox registry
- most knobs exposed in config

*if you wish to keep your brains, do mount them.*


## install

since this is mostly dev-focused, the recommended install is from source:

```sh
cargo install --git https://github.com/intfish/svamm
```


## setup

A typical setup:

```sh
# first: check the system requirements and write initial configs
svamm setup

# then customize ~/.config/svamm/config.toml

# (optional) build your own dev vm image
svamm image build /path/to/image --name my/dev:latest

# outputs a reference to use in configs
```


### completions setup

The shell completions cover both the `svamm` command and its `svm` abbreviation.
`svamm setup` installs scripts for both names and creates the `svm` symlink.
The scripts complete live sandbox names for the commands that take one.
They fall back to the static completions when the backend is unreachable.

The recommended way is to let `svamm setup` install the completions:

1. `svamm setup` detects the login shell.
2. It writes the scripts to the standard per-user location.
3. It prints the rc lines to add.

`svamm setup` never edits shell rc files.

To install the completions manually, generate the script. Redirect it into a directory that your shell loads:

```sh
# zsh: add the fpath directory to fpath before compinit
svamm completions zsh > ~/.local/share/zsh/site-functions/_svamm
svamm completions zsh --name svm > ~/.local/share/zsh/site-functions/_svm

# bash: bash-completion (v2) auto-loads this file
svamm completions bash > ~/.local/share/bash-completion/completions/svamm.bash
svamm completions bash --name svm > ~/.local/share/bash-completion/completions/svm.bash

# fish: fish auto-loads this file
svamm completions fish > ~/.config/fish/completions/svamm.fish
svamm completions fish --name svm > ~/.config/fish/completions/svm.fish
```

Supported shells: bash, zsh, fish.


## why

because docker's `sbx` doesn't work without being online and forces you to dox yourself.
for running a vm locally.


## in depth

### configuration

Settings live under the XDG config directory.

`~/.config/svamm/config.toml` holds the svamm settings. `svamm setup` writes it. Every value has a matching `SVAMM_*` environment variable.

`~/.config/svamm/` holds the msb-facing configurations:
- `*.yml` files are msb configurations.
- `net-rules.json` holds the network policy definitions.

`[images]` table
- `images.vm` is the sandbox vm image (**required**)
    - default: `ghcr.io/intfish/svamm-images/svamm:latest`
    - override: `SVAMM_IMAGES_VM`
- `images.registry` is the local registry sandbox image (**required**)
    - default: `ghcr.io/intfish/svamm-images/registry:latest`
    - override: `SVAMM_IMAGES_REGISTRY`

`[sandbox]` table (sandbox creation)
- `sandbox.root_disk_size` - the flat root disk size
    - default: `32G`
    - override: `SVAMM_ROOT_DISK_SIZE`
- `sandbox.workspace_quota` - the guest-write quota for rw workspace mounts
    - default: `32G`
    - override: `SVAMM_WORKSPACE_QUOTA`
- `sandbox.tmpfs_dest` - the guest destination of the tmpfs mount.
    - default: `/tmp`
- `sandbox.tmpfs_size` - the size of the guest tmpfs
    - default: `2G`
    - override: `SVAMM_TMPFS_SIZE`
- `sandbox.cpus` and `sandbox.memory` set the effective allocation
    - overrides: `SVAMM_CPUS`, `SVAMM_MEMORY`
- `sandbox.max_cpus` and `sandbox.max_memory` set the burstable ceiling
    - overrides: `SVAMM_MAX_CPUS`, `SVAMM_MAX_MEMORY`

The create flags `--root-disk-size`, `--tmpfs-size`, `--quota`, `--cpus`, `--memory`, `--max-cpus`, and `--max-memory` override the defaults for one sandbox.

Mutable state lives under `~/.local/state/svamm/`.
- Registry data lives in `~/.local/state/svamm/registry`.
- Per-sandbox state lives in `~/.local/state/svamm/state`.
- Microsandbox state lives in `~/.microsandbox/`.

### project `svamm.json`

A `svamm.json` at the root of the main rw mount can override per-project settings.
Inside the sandbox its path is masked with an empty read-only file by default, so guest processes
can neither read nor change the project settings. Set `"svamm_json": "readonly"` to mount the real
file read-only instead, so guests can read it.

- `svamm_json` - controls the in-sandbox view of this file:
    - `"masked"` (default) mounts an empty read-only facade so guests cannot read the real config;
    - `"readonly"` mounts the real file read-only.
- `mounts` list adds mounts to the `--rw` and `--ro` create flags. Each entry is an object with `src`, `dest`, and `mode`:
    - `src` - the host path
    - `mode` - `rw` or `ro`
    - `dest` - guest destination (optional); defaults to `/home/agent/<basename>`
    - the `rw` entries get the same guest-write quota as flag-provided mounts
    - removing the `mounts` object (or setting it to `[]`) drops the previously applied mounts
- `network.allow` and `network.deny` add project net rules to the sandbox policy
- `network.ports` (or top-level `ports`) publishes ports
- `envs` object sets guest environment variables
    - each key is a variable name, each value is the value to set
    - a value of the form `"${NAME}"` is resolved from the host environment when the command runs
    - the referenced name must be `[A-Za-z_][A-Za-z0-9_]*`; an invalid name, or an unset or empty host
      variable, fails the command
    - the name `SVAMM_REGISTRY_PROXY_PORT` is always generated and cannot be overridden by this
    - the reconcile diff does not show the values of interpolated keys, since the value may be a secret
    - removing the `envs` object (or setting it to `{}`) drops the previously applied variables
    - vars from the image or base config are never touched
- `image` replaces the default svamm image as the sandbox rootfs for this project only
    - the override applies at `svamm create` time; changing it requires recreating the sandbox

All of these settings are re-applied on sandbox restart except `image`.

### custom vm images

The public vm base image is on ghcr.io. The svamm tools help you build a custom layer on top of it.

```sh
# fetch the base image into the local docker daemon as svamm:<tag>
svamm image pull

# scaffold a custom image project (a Dockerfile FROM svamm:<tag>)
svamm image new ./my-agent

# build it and push it to the local registry
svamm image build ./my-agent --name acme/agent:latest

# rebuild without the docker build cache
svamm image build ./my-agent --name acme/agent:latest --no-cache

# list the images that the local registry stores
svamm image ls

# remove an image from the local registry and the msb cache
svamm image rm acme/agent:latest
```

`svamm image build` prints the resulting reference. `svamm image ls` lists one row per `repo:tag`.
The `SIZE` column shows the stored manifest size (config plus layers); a `-` means it could not be read.
The `PULL` column shows the sandbox-facing pull uri `svamm.internal:<proxy_port>/<repo>:<tag>`.

`svamm image rm <repo>:<tag>` deletes the image from the local registry and the msb cache.
If an existing sandbox still references the image, svamm removes nothing and names the sandboxes that use it.

To configure the image globally:

```toml
[images]
vm = "127.0.0.1:<registry_port>/acme/agent:latest"
```

To configure it per project:

```json
{ "image": "127.0.0.1:<registry_port>/acme/agent:latest" }
```

### mirroring images for sandboxes

To make an image available inside sandboxes without network rules or credentials, mirror it into the local registry.

```sh
svamm image mirror docker.io/library/postgres:19
# inside a sandbox: docker pull svamm.internal:<proxy_port>/library/postgres:19
```

`svamm image mirror` without arguments mirrors the configured vm base as `svamm:<tag>`. The `--as <name>` flag sets the name under which svamm stores the image.

### local image registry

svamm runs a private image registry as a dedicated sandbox combined image with nginx:
- the writable end is published on `127.0.0.1:<SVAMM_REGISTRY_PORT>` for host-only admin access
- for sandboxes a read-only proxy published on `127.0.0.1:<SVAMM_REGISTRY_PROXY_PORT>`
- svamm generates `host:tcp:<SVAMM_REGISTRY_PROXY_PORT>` into every sandbox's egress policy

usage:
- to pull images in a sandbox: `docker pull svamm.internal:<proxy_port>/<image>`.
- to push images from the host: `docker push 127.0.0.1:<registry_port>/<image>`.
- see `svamm image mirror -h` for convenience tooling

custom setups:
- `svamm setup` pulls the configured registry image into the microsandbox cache
- changing either registry port requires running `svamm setup` again.
- the registry data mount carries a guest-write quota (default `32G`).
    - raise `SVAMM_REGISTRY_QUOTA` or set `registry.quota` in `~/.config/svamm/config.toml`
    - run `svamm setup` to apply it, the data already stored is kept.
- commands that need the registry wait for it to answer for `registry.ready_timeout` milliseconds before failing (default `30000`).
    - raise `SVAMM_REGISTRY_READY_TIMEOUT_MS` when the registry vm boots slowly on your machine
    - the wait is bounded; a broken registry fails with the repair hint instead of hanging.

svamm creates or starts the registry sandbox automatically when it is missing or stopped.
svamm does this when you create, start, or run a sandbox, and when an image build, mirror, or ls needs to push or pull.
the registry sandbox is svamm-internal: `svamm list` does not include it, and `svamm stop -a` leaves it running.
if the registry cannot start, the vm still starts and svamm prints a warning.
`svamm setup` repairs the registry.

### reaching the host from a sandbox

`svamm.internal` resolves to the sandbox host from guest processes and docker containers.
- Guest processes resolve it through `/etc/hosts`.
- Docker containers resolve it through a dnsmasq instance that the svamm image runs inside the guest.

Egress policy still applies. To reach host services beyond the registry proxy, add a matching `network.allow` rule, for example `host:tcp:5432`.

### burstable VMs

`cpus` and `memory` set the effective allocation. `max_cpus` and `max_memory` set the burstable ceiling.
By default the two values are identical, and the sandbox does not burst.
To create a burstable vm, set the ceiling above the effective allocation:

```sh
# 4 cpus and 8 GiB effective, bursting up to 8 cpus and 16 GiB
svamm create dev . --cpus 4 --memory 8G --max-cpus 8 --max-memory 16G
```

### building and testing

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --all -- --check
```
