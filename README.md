# PocketGPU agent

The open-source agent behind [PocketGPU](https://pocketgpu.app): a small program for your GPU
machine that shares live GPU stats, job alerts, quick actions, a terminal and your local
models with your phone, with no network setup.

The agent connects to paired phones directly by device key using [iroh](https://iroh.computer),
falling back to an encrypted relay. It never runs as root, only paired devices may connect, and
every request is checked against that device's permission. Your stats, logs, terminal traffic
and chats stay on your machines; the push server only relays short alert text.

## Install

Linux and Apple silicon Macs:

```sh
curl -fsSL https://pocketgpu.app/install | sh
```

Windows 10 and 11 (PowerShell):

```powershell
irm https://pocketgpu.app/install.ps1 | iex
```

Both scripts are in [`install/`](install). Signed binaries are published at
[pocketgpu/pocketgpu-releases](https://github.com/pocketgpu/pocketgpu-releases); `SHA256SUMS`
is signed with minisign, and `pgpu update` checks that signature before it installs anything.
There is also a Docker image for rented GPUs: see [`docker/agent.Dockerfile`](docker/agent.Dockerfile).

## What's here

| Crate | What it is |
| --- | --- |
| [`crates/agent`](crates/agent) | The agent and the `pocketgpu` / `pgpu` command line |
| [`crates/proto`](crates/proto) | The protocol between agent and apps |
| [`crates/client`](crates/client) | The client side of the protocol, used by the apps |

The phone and desktop apps and our servers are not open source.

## Build

Rust 1.97 or newer:

```sh
cargo build --release -p pocketgpu
PGPU_FAKE_GPU=1 cargo test --workspace   # no NVIDIA GPU needed for the tests
```

## About this repository

This repository is published automatically from PocketGPU's main repository; each commit is a
snapshot. Issues are welcome. Pull requests can't be merged here directly, but good ones are
applied upstream and show up in the next snapshot. Report security issues to
security@pocketgpu.app rather than in a public issue.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
