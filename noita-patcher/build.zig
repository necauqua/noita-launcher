const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.resolveTargetQuery(.{ .cpu_arch = .x86, .os_tag = .windows, .abi = .gnu });
    const optimize = b.standardOptimizeOption(.{});

    const win32 = b.dependency("win32", .{}).module("win32");

    b.installArtifact(b.addLibrary(.{
        .name = "noita-path-hook",
        .linkage = .dynamic,
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/hook.zig"),
            .optimize = optimize,
            .target = target,
            .imports = &.{.{ .name = "win32", .module = win32 }},
        }),
    }));

    const exe = b.addExecutable(.{
        .name = "noita-trampoline",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .optimize = optimize,
            .target = target,
            .imports = &.{.{ .name = "win32", .module = win32 }},
        }),
    });
    exe.subsystem = .Windows; // hides the terminal
    b.installArtifact(exe);
}
