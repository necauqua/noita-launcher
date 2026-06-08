# noita-patcher

The native (Zig) component of noita-launcher. A single `zig build` produces both
32-bit Windows artifacts used to give each Noita instance its own isolated save
directory:

- `noita-path-hook.dll` — injected into the game process; redirects save/data
  path lookups.
- `noita-trampoline.exe` — spawns `noita.exe` suspended, injects the DLL, then
  resumes the game.

Both target `x86-windows-gnu` because Noita is a 32-bit Windows executable.

## noita-path-hook.dll

Noita stores its save data under the Windows `LocalLow` folder, resolved at
runtime via `SHGetKnownFolderPath(FOLDERID_LocalAppDataLow, ...)`. This DLL
intercepts that call using an **IAT (Import Address Table) patch** and returns a
caller-supplied path instead, so each launcher-managed instance sees its own
private `LocalLow` directory.

The hook is installed by calling the exported
`install(path: [*:0]const u8) bool` function after the DLL is loaded into the
target process. The path is stored as UTF-16 and returned via a
`CoTaskMemAlloc`-allocated buffer, matching the ownership contract of the
original API.

## noita-trampoline.exe

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

## Build

This subproject is built automatically by the `noita-launcher` Cargo build
script (`build.rs`) using `zig build`. You do not need to build it manually.

To build standalone (requires Zig 0.16.0):

```sh
zig build -Doptimize=ReleaseSafe
```

Outputs:

- `zig-out/bin/noita-path-hook.dll`
- `zig-out/bin/noita-trampoline.exe`

## Debugging

Debug messages are emitted via `OutputDebugStringA`, prefixed with
`[noita-path-hook]` / `[noita-trampoline]`. On Linux (Wine), these are visible by
setting `WINEDEBUG=-all,+debugstr`.

## Part of noita-launcher

This subproject is one component of
[noita-launcher](https://github.com/necauqua/noita-launcher). The two artifacts
work together so that separate instances read and write independent save
directories.

## AI use

Claude was used for research during development and wrote the readme you're
reading right now.
