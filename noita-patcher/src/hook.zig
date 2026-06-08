const std = @import("std");

const Guid = @import("win32").zig.Guid;
const win = @import("win32").everything;

pub const std_options = std.Options{ .logFn = @import("log.zig").mkLog("noita-path-hook") };

pub export fn DllMain(_: ?win.HINSTANCE, reason: u32, _: ?*anyopaque) callconv(.winapi) std.os.windows.BOOL {
    if (reason != win.DLL_PROCESS_ATTACH) {
        return .TRUE;
    }
    std.log.debug("DLL_PROCESS_ATTACH received", .{});

    // in DllMain we just pin ourselves to prevent someone unloading us (hyper unlikely but meh whatever)
    var module: ?win.HINSTANCE = null;
    const flags = win.GET_MODULE_HANDLE_EX_FLAG_PIN | win.GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS;
    if (win.GetModuleHandleExW(flags, @ptrCast(@alignCast(&DllMain)), &module) == win.FALSE) {
        std.log.debug("failed to pin DLL: {d}", .{@intFromEnum(win.GetLastError())});
    } else {
        std.log.debug("pinned self", .{});
    }
    return .TRUE;
}

export fn install(path: [*:0]const u8) bool {
    installHook(path) catch |e| {
        std.log.debug("failed to install hook: {s}", .{@errorName(e)});
        return false;
    };
    std.log.debug("hook installed, redirecting LocalLow to {f}", .{std.unicode.fmtUtf16Le(redirectPath)});
    return true;
}

fn installHook(path: [*:0]const u8) !void {
    const S = struct {
        var buf = std.mem.zeroes([1024:0]u16);
    };
    const wideLen = try std.unicode.utf8ToUtf16Le(S.buf[0 .. S.buf.len - 1], std.mem.span(path));
    redirectPath = S.buf[0..wideLen :0];

    original = try patchIat("shell32.dll", "SHGetKnownFolderPath", &detour);
}

var redirectPath: [:0]u16 = undefined;
var original: *const @TypeOf(detour) = undefined;

fn detour(rfid: *const Guid, dwFlags: u32, hToken: ?win.HANDLE, ppszPath: *win.PWSTR) callconv(.winapi) win.HRESULT {
    std.log.debug("detoured function called!", .{});

    if (!std.mem.eql(u8, &rfid.Bytes, &win.FOLDERID_LocalAppDataLow.Bytes)) {
        return original(rfid, dwFlags, hToken, ppszPath);
    }

    std.log.debug("redirecting LocalLow path to {f}", .{std.unicode.fmtUtf16Le(redirectPath)});

    // callers of SHGetKnownFolderPath are expected to call CoTaskMemFree on the
    // return, so we need to allocate our response through CoTaskMemAlloc to
    // match
    const dup: [*]u16 = @ptrCast(@alignCast(win.CoTaskMemAlloc((redirectPath.len + 1) * @sizeOf(u16)) orelse {
        return win.E_OUTOFMEMORY;
    }));
    @memcpy(dup[0..redirectPath.len], redirectPath[0..redirectPath.len]);
    dup[redirectPath.len] = 0;

    ppszPath.* = dup[0..redirectPath.len :0];

    return win.S_OK;
}

// this is half-vibecoded to have an IAT patch instead of minhook, which explodes big time on GE-proton :sad:
fn patchIat(targetDll: []const u8, targetName: []const u8, replacement: anytype) !@TypeOf(replacement) {
    comptime {
        const info = @typeInfo(@TypeOf(replacement));
        if (info != .pointer or @typeInfo(info.pointer.child) != .@"fn") {
            @compileError("expected a function pointer, got " ++ @typeName(@TypeOf(replacement)));
        }
    }

    const base: [*]u8 = @ptrCast(win.GetModuleHandleW(null) orelse return error.NoMainModule);

    var image = try std.coff.Coff.init(base[0..0x1000], true);
    const dir = image.getDataDirectories()[@intFromEnum(std.coff.IMAGE.DIRECTORY_ENTRY.IMPORT)];
    if (dir.virtual_address == 0) {
        return error.NoImportDir;
    }

    const desc = findImportDir(base, dir.virtual_address, targetDll) orelse return error.DllNotImported;
    if (desc.import_lookup_table_rva == 0) {
        return error.NoImportLookupTable;
    }

    const slot = findImportSlot(base, desc, targetName) orelse return error.FunctionNotImported;

    var prot: win.PAGE_PROTECTION_FLAGS = .{};
    if (win.VirtualProtect(slot, @sizeOf(usize), .{ .PAGE_READWRITE = 1 }, &prot) == win.FALSE) {
        return error.VirtualProtectFailed;
    }
    defer _ = win.VirtualProtect(slot, @sizeOf(usize), prot, &prot);

    const orig = slot.*;
    slot.* = @intFromPtr(replacement);
    return @ptrFromInt(orig);
}

fn findImportDir(base: [*]u8, table_rva: u32, targetDll: []const u8) ?*std.coff.ImportDirectoryEntry {
    var desc: [*]std.coff.ImportDirectoryEntry = @ptrCast(@alignCast(base + table_rva));
    while (desc[0].name_rva != 0) : (desc += 1) {
        const dll = std.mem.span(@as([*:0]const u8, @ptrCast(base + desc[0].name_rva)));
        if (std.ascii.eqlIgnoreCase(dll, targetDll)) {
            return &desc[0];
        }
    }
    return null;
}

fn findImportSlot(base: [*]u8, desc: *std.coff.ImportDirectoryEntry, targetName: []const u8) ?*usize {
    const lookup: [*]u32 = @ptrCast(@alignCast(base + desc.import_lookup_table_rva));
    const iat: [*]usize = @ptrCast(@alignCast(base + desc.import_address_table_rva));

    var i: usize = 0;
    while (lookup[i] != 0) : (i += 1) {
        const by_name = std.coff.ImportLookupEntry32.getImportByName(lookup[i]) orelse continue;
        const entry: *std.coff.ImportHintNameEntry = @ptrCast(@alignCast(base + by_name.name_table_rva));
        const name = std.mem.span(@as([*:0]const u8, @ptrCast(&entry.name)));
        if (std.mem.eql(u8, name, targetName)) {
            return &iat[i];
        }
    }
    return null;
}
