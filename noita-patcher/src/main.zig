const std = @import("std");

const win = @import("win32").everything;

const InstallArgs = @import("shared.zig").InstallArgs;

/// Load the DLL locally to get the proc address and then rebase it on top of the remote base
fn remoteProcAddress(dllPath: [*:0]const u16, module: win.HINSTANCE, procName: [*:0]const u8) ?*const anyopaque {
    const local = win.LoadLibraryExW(dllPath, null, .{ .DONT_RESOLVE_DLL_REFERENCES = 1 }) orelse return null;
    defer _ = win.FreeLibrary(local);

    const localProc = win.GetProcAddress(local, procName) orelse return null;
    const rva = @intFromPtr(localProc) - @intFromPtr(local);
    return @ptrFromInt(@intFromPtr(module) + rva);
}

/// Runs a remote thread at `start` with a single pointer arg, waits, returns exit code.
fn callRemote(proc: win.HANDLE, lpStartAddress: ?win.LPTHREAD_START_ROUTINE, lpParameter: ?*anyopaque) u32 {
    const thread = win.CreateRemoteThread(proc, null, 0, lpStartAddress, lpParameter, 0, null) orelse {
        fail("CreateRemoteThread");
    };
    defer _ = win.CloseHandle(thread);

    if (win.WaitForSingleObject(thread, win.INFINITE) != .NO_ERROR) {
        fail("WaitForSingleObject(remote thread)");
    }

    var exit_code: u32 = 0;
    if (win.GetExitCodeThread(thread, &exit_code) == 0) {
        fail("GetExitCodeThread");
    }
    return exit_code;
}

/// Calls LoadLibraryW(dll_path) in the target process
fn loadLibraryRemote(proc: win.HANDLE, dll_path: [:0]const u16) !win.HINSTANCE {
    const k32 = win.GetModuleHandleA("kernel32.dll") orelse {
        return error.NoKernel32;
    };
    const loadLibrary = win.GetProcAddress(k32, "LoadLibraryW") orelse {
        return error.NoLoadLibraryW;
    };

    var remoteArg = try RemoteMem.wstr(proc, dll_path);
    defer remoteArg.deinit();

    // All same-arch processes share the same kernel32 and so the LoadLibraryW
    // address is the same as ours
    const exit_code = callRemote(proc, @ptrCast(loadLibrary), remoteArg.ptr);
    if (exit_code == 0) {
        return error.LoadLibraryWFailed;
    }
    std.log.debug("injected hook DLL", .{});
    return @ptrFromInt(exit_code); // this is quite meh and only works on 32-bit
}

pub const std_options = std.Options{ .logFn = @import("log.zig").mkLog("noita-trampoline") };

pub fn main(init: std.process.Init) void {
    win.ExitProcess(run(init) catch |e| fail(@errorName(e)));
}

fn run(init: std.process.Init) !u32 {
    const arena = init.arena.allocator();

    const argv = try init.minimal.args.toSlice(arena);
    if (argv.len < 4) {
        std.log.debug("bad args, expected: noita-trampoline.exe <noita-exe> <path-hook-dll> <save-path> [noita args...]", .{});
        return 1;
    }
    const trampolineExe = argv[0];
    const hookDll = argv[1];
    const savePath = argv[2];
    const noitaExe = argv[3];
    const noitaArgs = argv[4..];

    std.log.debug("hookDll: {s}", .{hookDll});
    std.log.debug("savePath: {s}", .{savePath});
    std.log.debug("noitaExe: {s}", .{noitaExe});
    std.log.debug("noitaArgs: {s}", .{try std.mem.join(arena, " ", noitaArgs)});

    // meh this is good enough
    const cmdline = try std.fmt.allocPrint(arena, "\"{s}\" {s}", .{
        noitaExe,
        try std.mem.join(arena, " ", noitaArgs),
    });

    std.log.debug("launching noita with command line: {s}", .{cmdline});

    const cmdlineWide = try std.unicode.utf8ToUtf16LeAllocZ(arena, cmdline);

    var si = std.mem.zeroes(win.STARTUPINFOW);
    si.cb = @sizeOf(win.STARTUPINFOW);
    si.dwFlags = .{ .FORCEOFFFEEDBACK = 1 }; // dont show cursor as busy

    var pi = std.mem.zeroes(win.PROCESS_INFORMATION);

    const app = std.unicode.utf8ToUtf16LeStringLiteral("noita.exe");

    if (win.CreateProcessW(app.ptr, cmdlineWide.ptr, null, null, 0, .{ .CREATE_SUSPENDED = 1 }, null, null, &si, &pi) == 0) {
        fail("CreateProcessW(noita.exe)");
    }

    errdefer _ = win.TerminateProcess(pi.hProcess, 1);
    defer _ = win.CloseHandle(pi.hThread);
    defer _ = win.CloseHandle(pi.hProcess);

    const proc = pi.hProcess orelse {
        fail("invalid process handle from CreateProcessW");
    };

    const hookDllWide = try std.unicode.utf8ToUtf16LeAllocZ(arena, hookDll);
    const handle = try loadLibraryRemote(proc, hookDllWide);

    const args = InstallArgs{
        .trampoline_path = try remotePath(arena, proc, trampolineExe),
        .save_path = try remotePath(arena, proc, savePath),
        .dll_path = try remotePath(arena, proc, hookDll),
    };
    var remoteArg = try RemoteMem.wrap(proc, std.mem.asBytes(&args));
    defer remoteArg.deinit();

    const installAddr = remoteProcAddress(hookDllWide, handle, "install") orelse {
        fail("failed to find install() in hook DLL");
    };

    if (callRemote(proc, @ptrCast(installAddr), remoteArg.ptr) == 0) {
        std.log.debug("remote hook installation failed", .{});
        return 1;
    }
    std.log.debug("hook DLL install() called successfully", .{});

    if (win.ResumeThread(pi.hThread) == 0xFFFFFFFF) {
        fail("ResumeThread(noita.exe main thread)");
    }
    std.log.debug("resumed noita.exe main thread", .{});

    // here we just wait for the process to exit, so free anything we used so far
    _ = init.arena.reset(.free_all);

    if (win.WaitForSingleObject(pi.hProcess, win.INFINITE) != .NO_ERROR) {
        fail("WaitForSingleObject(noita.exe)");
    }
    var code: u32 = 0;
    if (win.GetExitCodeProcess(pi.hProcess, &code) == 0) {
        fail("GetExitCodeProcess(noita.exe)");
    }
    std.log.debug("noita.exe exited with code {d}", .{code});
    return code;
}

fn fail(msg: []const u8) noreturn {
    std.log.debug("{s}, last error: {f}", .{ msg, @import("win32").zig.fmtError(@intFromEnum(win.GetLastError())) });
    std.process.exit(1);
}

fn remotePath(arena: std.mem.Allocator, proc: win.HANDLE, utf8: []const u8) !@import("shared.zig").Path {
    const wide = try std.unicode.utf8ToUtf16LeAllocZ(arena, utf8);
    // meh we just leak it and treat as static in the DLL
    const remote = try RemoteMem.wstr(proc, wide);
    return @import("shared.zig").Path{
        .ptr = @ptrCast(@alignCast(remote.ptr)), // ugh on this side the pointer is invalid ofc ew
        .len = wide.len,
    };
}

const RemoteMem = struct {
    proc: win.HANDLE,
    ptr: *anyopaque,
    size: usize,

    pub fn wrap(proc: win.HANDLE, data: []const u8) !RemoteMem {
        var mem = try RemoteMem.alloc(proc, data.len);
        errdefer mem.deinit();
        try mem.write(data);
        return mem;
    }

    pub fn wstr(proc: win.HANDLE, str: [:0]const u16) !RemoteMem {
        return .wrap(proc, std.mem.sliceAsBytes(std.mem.absorbSentinel(str)));
    }

    pub fn alloc(proc: win.HANDLE, size: usize) !RemoteMem {
        const ptr = win.VirtualAllocEx(
            proc,
            null,
            size,
            .{ .COMMIT = 1, .RESERVE = 1 },
            .{ .PAGE_READWRITE = 1 },
        ) orelse return error.VirtualAllocFailed;
        return .{ .proc = proc, .ptr = ptr, .size = size };
    }

    pub fn write(self: RemoteMem, data: []const u8) !void {
        var written: usize = 0;
        if (win.WriteProcessMemory(self.proc, self.ptr, data.ptr, data.len, &written) == win.FALSE) {
            return error.WriteProcessMemoryFailed;
        }
        if (written != data.len) {
            return error.WriteProcessMemoryShort;
        }
    }

    pub fn deinit(self: *RemoteMem) void {
        _ = win.VirtualFreeEx(self.proc, self.ptr, 0, .RELEASE);
    }
};
