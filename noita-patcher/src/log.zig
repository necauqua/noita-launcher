const std = @import("std");

const win = @import("win32").everything;

pub fn mkLog(comptime prefix: []const u8) @FieldType(std.Options, "logFn") {
    const S = struct {
        pub fn log(comptime level: std.log.Level, comptime scope: @EnumLiteral(), comptime format: []const u8, args: anytype) void {
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
            win.OutputDebugStringA(formatted.ptr);
        }
    };
    return S.log;
}
