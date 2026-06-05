# noita-trampoline

A 32-bit Windows executable that spawns `noita.exe` suspended, injects the
hook DLL, then resumes the game — ensuring path redirection is active before
any game code runs.

## What it does

The trampoline performs DLL injection before the game's main thread is allowed
to run:

1. Spawns `noita.exe` in a **suspended** state via `CreateProcess`, with the
   instance directory as the working directory.
2. Injects the hook DLL into the suspended process using `LoadLibraryW` called
   via `CreateRemoteThread`.
3. Calls the `install(save_path)` export from the hook DLL in the remote
   process, passing the per-instance save path as an argument.
4. Resumes the main thread of `noita.exe`.
5. Waits for `noita.exe` to exit and forwards its exit code.

```
noita-trampoline.exe <instance-dir> <noita-exe> <hook-dll> <save-path> [noita args...]
```

### Implementation notes

- `remoteProcAddress` resolves the RVA of `install` by loading the DLL
  locally, then rebases it onto the remote module handle. This only works
  reliably on 32-bit processes sharing the same address space layout.
- `kernel32.dll` is loaded at the same address in all same-architecture
  processes, so `LoadLibraryW` can be called directly without rebasing.
- The subsystem is set to `windows` (not `console`) so no terminal window
  appears when the launcher spawns the trampoline.
- Targets `x86-windows-gnu` — Noita is a 32-bit Windows executable.

## Build

This subproject is built automatically by the `noita-launcher` Cargo build
script (`build.rs`) using `zig build`. You do not need to build it manually.

To build standalone (requires Zig 0.16.0):

```sh
zig build -Doptimize=ReleaseSafe
```

Output: `zig-out/bin/noita-trampoline.exe`

## Debugging

Debug messages are emitted via `OutputDebugStringA` prefixed with
`[noita-trampoline]`. On Linux (Wine), these are visible by setting
`WINEDEBUG=-all,+debugstr`.

## Part of noita-launcher

This executable is one component of
[noita-launcher](https://github.com/necauqua/noita-launcher). It works
alongside [noita-path-hook](../noita-path-hook/README.md) to give each
instance its own isolated save directory.

## AI use

Claude was used for research during development and wrote the readme you're
reading right now.
