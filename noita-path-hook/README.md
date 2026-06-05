# noita-path-hook

A 32-bit Windows DLL that redirects Noita's save and data path lookups to an
arbitrary directory, enabling multiple isolated game instances to coexist.

## What it does

Noita stores its save data under the Windows `LocalLow` folder, resolved at
runtime via `SHGetKnownFolderPath(FOLDERID_LocalAppDataLow, ...)`. This DLL
intercepts that call using an **IAT (Import Address Table) patch** and returns
a caller-supplied path instead, so each launcher-managed instance sees its own
private `LocalLow` directory.

The hook is installed by calling the exported `install(path: [*:0]const u8) bool`
function after the DLL is loaded into the target process. The path is stored as
UTF-16 and returned via a `CoTaskMemAlloc`-allocated buffer, matching the
ownership contract of the original API.

### Implementation notes

- Uses IAT patching rather than inline hooking (MinHook is vendored but not
  used at runtime) because inline hooking via MinHook was found to be
  incompatible with GE-Proton.
- The DLL pins itself on attach (`GetModuleHandleExW` with
  `GET_MODULE_HANDLE_EX_FLAG_PIN`) to prevent premature unloading.
- Targets `x86-windows-gnu` — Noita is a 32-bit Windows executable.
- Depends on [MinHook v1.3.4](https://github.com/TsudaKageyu/minhook) (static,
  compile-time only; the Zig bindings in `src/minhook.zig` are unused at
  runtime but kept for reference).

## Build

This subproject is built automatically by the `noita-launcher` Cargo build
script (`build.rs`) using `zig build`. You do not need to build it manually.

To build standalone (requires Zig 0.16.0):

```sh
zig build -Doptimize=ReleaseSafe
```

Output: `zig-out/lib/noita-path-hook.dll`

## Part of noita-launcher

This DLL is one component of
[noita-launcher](https://github.com/necauqua/noita-launcher). The launcher
injects it into each Noita process alongside
[noita-trampoline](../noita-trampoline/README.md) so that separate instances
read and write independent save directories.

## AI use

Claude was used for research during development and wrote the readme you're
reading right now.
