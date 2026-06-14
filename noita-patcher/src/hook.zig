const std = @import("std");

const Guid = @import("win32").zig.Guid;
const win = @import("win32").everything;

const InstallArgs = @import("shared.zig").InstallArgs;

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

var args: InstallArgs = undefined;

export fn install(_args: *InstallArgs) bool {
    args = _args.*;
    std.log.debug("hook install called; trampoline_path={f}, save_path={f}, dll_path={f}", .{
        args.trampoline_path,
        args.save_path,
        args.dll_path,
    });
    installHook() catch |e| {
        std.log.debug("failed to install hook: {s}", .{@errorName(e)});
        return false;
    };
    return true;
}

fn installHook() !void {
    try patchIat("shell32.dll", "SHGetKnownFolderPath", struct {
        var original: *const @TypeOf(win.SHGetKnownFolderPath) = undefined;

        fn detour(
            rfid: ?*const Guid,
            dwFlags: u32,
            hToken: ?win.HANDLE,
            ppszPath: ?*?win.PWSTR,
        ) callconv(.winapi) win.HRESULT {
            std.log.debug("detoured SHGetKnownFolderPath called", .{});

            if (rfid == null or !std.mem.eql(u8, &rfid.?.Bytes, &win.FOLDERID_LocalAppDataLow.Bytes)) {
                return original(rfid, dwFlags, hToken, ppszPath);
            }

            std.log.debug("redirecting LocalLow path to {f}", .{args.save_path});

            // callers of SHGetKnownFolderPath are expected to call CoTaskMemFree on the
            // return, so we need to allocate our response through CoTaskMemAlloc to
            // match
            const dup: [*]u16 = @ptrCast(@alignCast(win.CoTaskMemAlloc((args.save_path.len + 1) * @sizeOf(u16)) orelse {
                return win.E_OUTOFMEMORY;
            }));
            @memcpy(dup[0..args.save_path.len], args.save_path.span());
            dup[args.save_path.len] = 0;

            if (ppszPath) |out| {
                out.* = dup[0..args.save_path.len :0];
            }

            return win.S_OK;
        }
    });

    try patchIat("kernel32.dll", "CreateProcessW", struct {
        var original: *const @TypeOf(win.CreateProcessW) = undefined;

        fn detour(
            lpApplicationName: ?[*:0]const u16,
            lpCommandLine: ?win.PWSTR,
            lpProcessAttributes: ?*win.SECURITY_ATTRIBUTES,
            lpThreadAttributes: ?*win.SECURITY_ATTRIBUTES,
            bInheritHandles: win.BOOL,
            dwCreationFlags: win.PROCESS_CREATION_FLAGS,
            lpEnvironment: ?*anyopaque,
            lpCurrentDirectory: ?[*:0]const u16,
            lpStartupInfo: ?*win.STARTUPINFOW,
            lpProcessInformation: ?*win.PROCESS_INFORMATION,
        ) callconv(.winapi) win.BOOL {
            std.log.debug("detoured CreateProcessW called", .{});

            var buf: [1024]u16 = undefined;
            var patchedAppName = lpApplicationName;
            var patchedCmdline = lpCommandLine;

            if (lpApplicationName) |appName| {
                if (lpCommandLine) |cmdline| {
                    if (std.mem.eql(u16, std.mem.span(appName), std.unicode.utf8ToUtf16LeStringLiteral("noita.exe"))) {
                        const res = w: {
                            var w = std.Io.Writer.fixed(std.mem.sliceAsBytes(&buf));
                            const space = std.mem.sliceAsBytes(&[_]u16{' '});
                            w.writeAll(args.trampoline_path.bytes()) catch |e| break :w e;
                            w.writeAll(space) catch |e| break :w e;
                            w.writeAll(args.dll_path.bytes()) catch |e| break :w e;
                            w.writeAll(space) catch |e| break :w e;
                            w.writeAll(args.save_path.bytes()) catch |e| break :w e;
                            w.writeAll(space) catch |e| break :w e;
                            w.writeAll(std.mem.sliceAsBytes(std.mem.span(cmdline))) catch |e| break :w e;
                            w.writeAll(std.mem.sliceAsBytes(&[_]u16{0})) catch |e| break :w e;
                        };
                        res catch |e| {
                            std.log.debug("failed to build patched command line: {s}", .{@errorName(e)});
                            std.process.exit(1);
                        };

                        patchedCmdline = @ptrCast(&buf);
                        patchedAppName = args.trampoline_path.ptr;

                        std.log.debug("patched cmdline: {f}", .{std.unicode.fmtUtf16Le(std.mem.span(patchedCmdline.?))});
                    }
                }
            }

            return original(
                patchedAppName,
                patchedCmdline,
                lpProcessAttributes,
                lpThreadAttributes,
                bInheritHandles,
                dwCreationFlags,
                lpEnvironment,
                lpCurrentDirectory,
                lpStartupInfo,
                lpProcessInformation,
            );
        }
    });
}

fn patchIat(targetDll: []const u8, targetName: []const u8, stuff: anytype) !void {
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
    stuff.original = @ptrFromInt(orig);
    slot.* = @intFromPtr(&stuff.detour);
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
