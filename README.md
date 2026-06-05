# noita-launcher

A CLI (for now; a GUI is an immediate todo) tool for managing multiple isolated
instances and saves of [Noita](https://noitagame.com/).

Downloads game files directly from Steam depots without requiring the Steam
client to be running, allowing to run any historial version of the game with
ease.

## What it does

- Authenticates with Steam via the Steam network protocol and downloads any
  historical Noita version by depot manifest ID.
- Stores downloaded depot chunks in a local cache so switching between versions
  avoids redundant downloads.
- Runs each instance in its own directory with its own save data, isolated from
  other instances and from the normal Steam installation.
- On Linux, launches Noita through
  [umu-launcher](https://github.com/Open-Wine-Components/umu-launcher)
  (`umu-run`).

Two small Zig subprojects handle the instance isolation at the process level:

- [`noita-path-hook`](noita-path-hook/README.md) — a 32-bit Windows DLL injected
  into each Noita process that redirects save path lookups to the per-instance
  directory.
- [`noita-trampoline`](noita-trampoline/README.md) — a 32-bit Windows executable
  that spawns `noita.exe` suspended, injects the hook DLL, then resumes the
  game.

Both are compiled by `build.rs` via `zig build` and embedded into the launcher
binary; they are extracted to the data directory on first run.

### Why Zig

It is a huge pain in the ass to cross-compile a 32-bit exe/dll using Rust while
being on 64-bit NixOS (in fact I never got it working), and Zig literally just
worked first try, like magic.

Additionally, while Rust is fantastic for more app-level stuff (and the `clap`
crate is amazing), doing patches and winapi calls like we need there would be
quite clunky in Rust, while in Zig its quite natural.

## Building

Requires [Rust](https://rustup.rs/) 1.92.0+ and [Zig](https://ziglang.org/)
0.16.0 (for the two bundled subprojects).

```sh
cargo build # --release
```

The build script compiles and embeds `noita-trampoline` and `noita-path-hook`
automatically.

## Known versions

The embedded `src/manifests.toml` contains a catalogue of historical Noita depot
manifests going back to the initial release on 24 September 2019, manually
scraped from [SteamDB](https://steamdb.info/depot/881101/manifests/) - btw
unrelated I donated a copy of Noita to their bot. This list is only used by
`prefetch-all` — any manifest ID can be passed directly to `new` or `prefetch`
to create or cache an arbitrary version.

## AI use

Claude was used for research during development and wrote the readme you're
reading right now.
