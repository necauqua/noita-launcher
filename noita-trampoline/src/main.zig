const std = @import("std");

/// this windows codegen where they split all of winapi into a billion namespaces is annoying as all fuck ughghgh
const win32 = @import("win32");
const t = win32.system.threading;
const lib = win32.system.library_loader;
const CloseHandle = win32.foundation.CloseHandle;
const GetLastError = win32.foundation.GetLastError;
const OutputDebugStringA = win32.system.diagnostics.debug.OutputDebugStringA;
const WriteProcessMemory = win32.system.diagnostics.debug.WriteProcessMemory;
const INFINITE = win32.system.windows_programming.INFINITE;
const HANDLE = win32.foundation.HANDLE;
const HINSTANCE = win32.foundation.HINSTANCE;

/// Load the DLL locally to get the proc address and then rebase it on top of the remote base
fn remoteProcAddress(dllPath: [*:0]const u16, module: HINSTANCE, procName: [*:0]const u8) ?*const anyopaque {
    const local = lib.LoadLibraryExW(dllPath, null, .{ .DONT_RESOLVE_DLL_REFERENCES = 1 }) orelse return null;
    defer _ = lib.FreeLibrary(local);

    const localProc = lib.GetProcAddress(local, procName) orelse return null;
    const rva = @intFromPtr(localProc) - @intFromPtr(local);
    return @ptrFromInt(@intFromPtr(module) + rva);
}

/// Runs a remote thread at `start` with a single pointer arg, waits, returns exit code.
fn callRemote(proc: HANDLE, lpStartAddress: ?t.LPTHREAD_START_ROUTINE, lpParameter: ?*anyopaque) u32 {
    const thread = t.CreateRemoteThread(proc, null, 0, lpStartAddress, lpParameter, 0, null) orelse {
        fail("CreateRemoteThread");
    };
    defer _ = CloseHandle(thread);

    if (t.WaitForSingleObject(thread, INFINITE) != .NO_ERROR) {
        fail("WaitForSingleObject(remote thread)");
    }

    var exit_code: u32 = 0;
    if (t.GetExitCodeThread(thread, &exit_code) == 0) {
        fail("GetExitCodeThread");
    }
    return exit_code;
}

/// Calls LoadLibraryW(dll_path) in the target process
fn loadLibraryRemote(proc: HANDLE, dll_path: [:0]const u16) !HINSTANCE {
    const k32 = lib.GetModuleHandleA("kernel32.dll") orelse {
        return error.NoKernel32;
    };
    const loadLibrary = lib.GetProcAddress(k32, "LoadLibraryW") orelse {
        return error.NoLoadLibraryW;
    };

    var remoteArg = try RemoteMem.wrap(proc, std.mem.sliceAsBytes(std.mem.absorbSentinel(dll_path)));
    defer remoteArg.deinit();

    // All same-arch processes share the same kernel32 and so the LoadLibraryW
    // address is the same as ours
    const exit_code = callRemote(proc, @ptrCast(loadLibrary), remoteArg.ptr);
    if (exit_code == 0) {
        return error.LoadLibraryWFailed;
    }
    debug("injected hook DLL", .{});
    return @ptrFromInt(exit_code); // this is quite meh and only works on 32-bit
}

fn run(init: std.process.Init) !u32 {
    const arena = init.arena.allocator();

    const argv = try init.minimal.args.toSlice(arena);
    if (argv.len < 5) {
        debug("bad args, expected: noita-trampoline.exe <instance-dir> <noita-exe> <path-hook-dll> <save-path> [noita args...]", .{});
        return 1;
    }
    const instanceDir = argv[1];
    const noitaExe = argv[2];
    const hookDll = argv[3];
    const savePath = argv[4];
    const noitaArgs = argv[5..];

    debug("instanceDir: {s}", .{instanceDir});
    debug("noitaExe: {s}", .{noitaExe});
    debug("hookDll: {s}", .{hookDll});
    debug("savePath: {s}", .{savePath});
    debug("noitaArgs: {s}", .{try std.mem.join(arena, " ", noitaArgs)});

    // meh this is good enough
    const cmdline = try std.fmt.allocPrint(arena, "\"{s}\" {s}", .{
        noitaExe,
        try std.mem.join(arena, " ", noitaArgs),
    });

    debug("launching noita with command line: {s}", .{cmdline});

    const cmdlineWide = try std.unicode.utf8ToUtf16LeAllocZ(arena, cmdline);
    const instanceDirWide = try std.unicode.utf8ToUtf16LeAllocZ(arena, instanceDir);

    var si = std.mem.zeroes(t.STARTUPINFOW);
    si.cb = @sizeOf(t.STARTUPINFOW);
    var pi = std.mem.zeroes(t.PROCESS_INFORMATION);

    if (t.CreateProcessW(null, cmdlineWide.ptr, null, null, 0, .{ .CREATE_SUSPENDED = 1 }, null, instanceDirWide.ptr, &si, &pi) == 0) {
        fail("CreateProcessW(noita.exe)");
    }
    errdefer _ = t.TerminateProcess(pi.hProcess, 1);
    defer _ = CloseHandle(pi.hThread);
    defer _ = CloseHandle(pi.hProcess);

    const proc = pi.hProcess orelse {
        fail("invalid process handle from CreateProcessW");
    };

    const hookDllWide = try std.unicode.utf8ToUtf16LeAllocZ(arena, hookDll);
    const handle = try loadLibraryRemote(proc, hookDllWide);

    // basically just append 0
    const savePathArg = std.mem.absorbSentinel(try arena.dupeSentinel(u8, savePath, 0));
    var remoteArg = try RemoteMem.wrap(proc, savePathArg);
    defer remoteArg.deinit();

    const installAddr = remoteProcAddress(hookDllWide, handle, "install") orelse {
        fail("failed to find install() in hook DLL");
    };

    if (callRemote(proc, @ptrCast(installAddr), remoteArg.ptr) == 0) {
        debug("remote hook installation failed", .{});
        return 1;
    }
    if (t.ResumeThread(pi.hThread) == 0xFFFFFFFF) {
        fail("ResumeThread(noita.exe main thread)");
    }
    _ = init.arena.reset(.free_all); // here we just wait for the process to exit, so free anything we used so far
    if (t.WaitForSingleObject(pi.hProcess, INFINITE) != .NO_ERROR) {
        fail("WaitForSingleObject(noita.exe)");
    }
    var code: u32 = 0;
    if (t.GetExitCodeProcess(pi.hProcess, &code) == 0) {
        fail("GetExitCodeProcess(noita.exe)");
    }
    debug("noita.exe exited with code {d}", .{code});
    return code;
}

pub fn main(init: std.process.Init) void {
    t.ExitProcess(run(init) catch |e| fail(@errorName(e)));
}

fn fail(msg: []const u8) noreturn {
    debug("{s}, last error: {f}", .{ msg, win32.zig.fmtError(@intFromEnum(GetLastError())) });
    std.process.exit(1);
}

// On linux running with WINEDEBUG=-all,+debugstr shows those,
// no idea how to see them on windows
inline fn debug(comptime fmt: []const u8, args: anytype) void {
    var buf: [1024]u8 = undefined;
    const formatted = std.fmt.bufPrintSentinel(&buf, "[noita-trampoline] " ++ fmt, args, 0) catch blk: {
        const trunc = "...[truncated]\x00";
        @memcpy(buf[buf.len - trunc.len ..], trunc);
        break :blk buf[0 .. buf.len - 1 :0];
    };
    OutputDebugStringA(formatted.ptr);
}

const RemoteMem = struct {
    proc: HANDLE,
    ptr: *anyopaque,
    size: usize,

    pub fn wrap(proc: HANDLE, data: []const u8) error{ VirtualAllocFailed, WriteProcessMemoryFailed, WriteProcessMemoryShort }!RemoteMem {
        var mem = try RemoteMem.alloc(proc, data.len);
        errdefer mem.deinit();
        try mem.write(data);
        return mem;
    }

    pub fn alloc(proc: HANDLE, size: usize) error{VirtualAllocFailed}!RemoteMem {
        const ptr = win32.system.memory.VirtualAllocEx(
            proc,
            null,
            size,
            .{ .COMMIT = 1, .RESERVE = 1 },
            .{ .PAGE_READWRITE = 1 },
        ) orelse return error.VirtualAllocFailed;
        return .{ .proc = proc, .ptr = ptr, .size = size };
    }

    pub fn write(self: RemoteMem, data: []const u8) error{ WriteProcessMemoryFailed, WriteProcessMemoryShort }!void {
        var written: usize = 0;
        if (WriteProcessMemory(self.proc, self.ptr, data.ptr, data.len, &written) == 0) {
            return error.WriteProcessMemoryFailed;
        }
        if (written != data.len) {
            return error.WriteProcessMemoryShort;
        }
    }

    pub fn deinit(self: *RemoteMem) void {
        _ = win32.system.memory.VirtualFreeEx(self.proc, self.ptr, 0, .RELEASE);
    }
};
