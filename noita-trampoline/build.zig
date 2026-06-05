const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.resolveTargetQuery(.{ .cpu_arch = .x86, .os_tag = .windows, .abi = .gnu });
    const optimize = b.standardOptimizeOption(.{});

    const root = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .optimize = optimize,
        .target = target,
    });

    root.addImport("win32", b.dependency("win32", .{}).module("win32"));

    const exe = b.addExecutable(.{ .name = "noita-trampoline", .root_module = root });
    exe.subsystem = .Windows; // hides the terminal
    b.installArtifact(exe);
}
