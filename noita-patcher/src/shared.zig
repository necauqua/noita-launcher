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
