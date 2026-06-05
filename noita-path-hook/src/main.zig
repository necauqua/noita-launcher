const std = @import("std");
const win = std.os.windows;
const wide = std.unicode.utf8ToUtf16LeStringLiteral;

const minhook = @import("minhook.zig");

// meh only 4 externs used, no need to bring in the entire zigwin32 here
extern "ole32" fn CoTaskMemAlloc(cb: usize) callconv(.winapi) ?*anyopaque;
extern "kernel32" fn OutputDebugStringA(lpOutputString: win.LPCSTR) callconv(.{ .x86_stdcall = .{} }) void;
extern "kernel32" fn VirtualProtect(addr: win.LPVOID, size: usize, new: win.PAGE, old: *win.PAGE) callconv(.winapi) win.BOOL;
extern "kernel32" fn GetModuleHandleExW(dwFlags: packed struct(u32) {
    pin: u1 = 0,
    unchanged_refcount: u1 = 0,
    from_address: u1 = 0,
    _reserved: u29 = 0,
}, lpModuleName: ?win.LPCWSTR, phModule: *win.HMODULE) callconv(.winapi) win.BOOL;

// On linux running with WINEDEBUG=-all,+debugstr shows those,
// no idea how to see them on windows
fn debug(comptime fmt: []const u8, args: anytype) void {
    var buf: [1024]u8 = undefined;
    const formatted = std.fmt.bufPrintSentinel(&buf, "[noita-path-hook] " ++ fmt, args, 0) catch blk: {
        const trunc = "...[truncated]\x00";
        @memcpy(buf[buf.len - trunc.len ..], trunc);
        break :blk buf[0 .. buf.len - 1 :0];
    };
    OutputDebugStringA(formatted.ptr);
}

// callers of SHGetKnownFolderPath are expected to call CoTaskMemFree on the
// return, so we need to allocate our response through that as well
fn coTaskDupZ(s: [:0]const u16) ?win.PWSTR {
    const bytes = (s.len + 1) * @sizeOf(u16);
    const ptr = CoTaskMemAlloc(bytes) orelse return null;
    const out: [*]u16 = @ptrCast(@alignCast(ptr));
    @memcpy(out[0..s.len], s[0..s.len]);
    out[s.len] = 0;
    return out[0..s.len :0];
}

var redirectPath: [:0]u16 = undefined;
var original: *const @TypeOf(detour) = undefined;

const HRESULT = u32;

fn detour(rfid: *const win.GUID, dwFlags: u32, hToken: ?win.HANDLE, ppszPath: *win.PWSTR) callconv(.winapi) HRESULT {
    debug("detoured function called!", .{});

    const FOLDERID_LOCAL_APP_DATA_LOW = comptime win.GUID.parse("{A520A1A4-1780-4FF6-BD18-167343C5AF16}");
    if (!std.meta.eql(rfid.*, FOLDERID_LOCAL_APP_DATA_LOW)) {
        return original(rfid, dwFlags, hToken, ppszPath);
    }

    debug("redirecting LocalLow path to {f}", .{std.unicode.fmtUtf16Le(redirectPath)});

    const S_OK: HRESULT = 0;
    const E_OUTOFMEMORY: HRESULT = 0x8007000E;

    const dup = coTaskDupZ(redirectPath) orelse return E_OUTOFMEMORY;
    ppszPath.* = dup;
    return S_OK;
}

fn installHook(path: [*:0]const u8) !void {
    const S = struct {
        var buf = std.mem.zeroes([1024:0]u16);
    };
    const wideLen = try std.unicode.utf8ToUtf16Le(S.buf[0 .. S.buf.len - 1], std.mem.span(path));
    redirectPath = S.buf[0..wideLen :0];

    //     try minhook.initialize();
    //     try minhook.createHookApi(wide("shell32.dll"), "SHGetKnownFolderPath", &detour, &original);
    //     try minhook.enableAll();

    original = try patchIat("shell32.dll", "SHGetKnownFolderPath", &detour);
}

export fn install(path: [*:0]const u8) bool {
    installHook(path) catch |e| {
        debug("failed to install hook: {s}", .{@errorName(e)});
        return false;
    };
    debug("hook installed, redirecting LocalLow to {f}", .{std.unicode.fmtUtf16Le(redirectPath)});
    return true;
}

pub export fn DllMain(_: ?win.HINSTANCE, reason: win.DWORD, _: ?*anyopaque) callconv(.winapi) win.BOOL {
    const DLL_PROCESS_ATTACH = 1;
    if (reason == DLL_PROCESS_ATTACH) {
        debug("DLL_PROCESS_ATTACH received", .{});
        pinSelf();
        debug("pinned self", .{});
    }
    return .TRUE;
}

fn pinSelf() void {
    var module: win.HMODULE = undefined;
    const ok = GetModuleHandleExW(.{ .pin = 1, .from_address = 1 }, @ptrCast(@alignCast(&pinSelf)), &module);
    if (!ok.toBool()) {
        debug("failed to pin DLL: {d}", .{@intFromEnum(win.GetLastError())});
    }
}

// this is half-vibecoded to have an IAT patch instead of minhook, which explodes big time on GE-proton :sad:
fn patchIat(targetDll: []const u8, targetName: []const u8, replacement: anytype) !@TypeOf(replacement) {
    comptime {
        const info = @typeInfo(@TypeOf(replacement));
        if (info != .pointer or @typeInfo(info.pointer.child) != .@"fn") {
            @compileError("expected a function pointer, got " ++ @typeName(@TypeOf(replacement)));
        }
    }

    var module: win.HMODULE = undefined;
    if (!GetModuleHandleExW(.{}, null, &module).toBool()) {
        return error.NoMainModule;
    }
    const base: [*]u8 = @ptrCast(module);

    var image = try std.coff.Coff.init(base[0..0x1000], true);
    const dir = image.getDataDirectories()[@intFromEnum(std.coff.IMAGE.DIRECTORY_ENTRY.IMPORT)];
    if (dir.virtual_address == 0) {
        return error.NoImportDir;
    }

    var desc: [*]std.coff.ImportDirectoryEntry = @ptrCast(@alignCast(base + dir.virtual_address));

    while (desc[0].name_rva != 0) : (desc += 1) {
        const dll = std.mem.span(@as([*:0]const u8, @ptrCast(base + desc[0].name_rva)));

        if (!std.ascii.eqlIgnoreCase(dll, targetDll)) {
            continue;
        }

        const lookup_rva =
            if (desc[0].import_lookup_table_rva != 0)
                desc[0].import_lookup_table_rva
            else
                desc[0].import_address_table_rva;

        const lookup: [*]u32 = @ptrCast(@alignCast(base + lookup_rva));
        const iat: [*]usize = @ptrCast(@alignCast(base + desc[0].import_address_table_rva));

        var i: usize = 0;
        while (lookup[i] != 0) : (i += 1) {
            const by_name = std.coff.ImportLookupEntry32.getImportByName(lookup[i]) orelse continue;
            // name table entry: u16 Hint, then the ASCII name
            const name = std.mem.span(@as([*:0]const u8, @ptrCast(base + by_name.name_table_rva + 2)));
            if (!std.mem.eql(u8, name, targetName)) {
                continue;
            }

            const slot = &iat[i];
            var prot: win.PAGE = .{};
            if (!VirtualProtect(slot, @sizeOf(usize), .{ .READWRITE = true }, &prot).toBool()) {
                return error.VirtualProtectFailed;
            }
            defer _ = VirtualProtect(slot, @sizeOf(usize), prot, &prot);

            const orig = slot.*;
            slot.* = @intFromPtr(replacement);
            return @ptrFromInt(orig);
        }
        return error.FunctionNotImported;
    }
    return error.DllNotImported;
}
