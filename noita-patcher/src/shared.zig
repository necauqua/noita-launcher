const std = @import("std");

pub const Path = extern struct {
    ptr: [*:0]const u16,
    len: usize,

    pub fn from(slice: [:0]const u16) Path {
        return .{ .ptr = slice.ptr, .len = slice.len };
    }

    pub fn span(self: Path) [:0]const u16 {
        return self.ptr[0..self.len :0];
    }

    pub fn bytes(self: Path) []const u8 {
        return std.mem.sliceAsBytes(self.span());
    }

    pub fn format(self: Path, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        try writer.print("{f}", .{std.unicode.fmtUtf16Le(self.span())});
    }
};

pub const InstallArgs = extern struct {
    trampoline_path: Path,
    dll_path: Path,
    save_path: Path,
};

pub fn mkLog(comptime prefix: []const u8) @FieldType(std.Options, "logFn") {
    return struct {
        fn log(
            comptime level: std.log.Level,
            comptime scope: @EnumLiteral(),
            comptime format: []const u8,
            args: anytype,
        ) void {
            if (level != .debug) {
                return std.log.defaultLog(level, scope, format, args);
            }

            var buf: [1024]u8 = undefined;
            const formatted = std.fmt.bufPrintSentinel(&buf, "[" ++ prefix ++ "] " ++ format, args, 0) catch blk: {
                const trunc = "...[truncated]\x00";
                @memcpy(buf[buf.len - trunc.len ..], trunc);
                break :blk buf[0 .. buf.len - 1 :0];
            };

            // on linux running with WINEDEBUG=-all,+debugstr shows those calls,
            // no idea how to see them on windows
            @import("win32")
                .system.diagnostics.debug
                .OutputDebugStringA(formatted.ptr);
        }
    }.log;
}
