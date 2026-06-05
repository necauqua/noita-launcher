const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.resolveTargetQuery(.{
        .cpu_arch = .x86,
        .os_tag = .windows,
        .abi = .gnu,
    });
    const optimize = b.standardOptimizeOption(.{});

    const minhook_dep = b.dependency("minhook", .{
        .target = target,
        .optimize = optimize,
    });

    const minhook_mod = b.createModule(.{
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    minhook_mod.addCSourceFiles(.{
        .files = &.{
            "src/buffer.c",
            "src/hook.c",
            "src/trampoline.c",
            "src/hde/hde32.c",
        },
        .root = minhook_dep.path("."),
    });
    minhook_mod.addIncludePath(minhook_dep.path("include"));

    const minhook_lib = b.addLibrary(.{
        .name = "minhook",
        .root_module = minhook_mod,
        .linkage = .static,
    });

    const lib = b.addLibrary(.{
        .name = "noita-path-hook",
        .linkage = .dynamic,
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .optimize = optimize,
            .target = target,
        }),
    });
    lib.root_module.linkLibrary(minhook_lib);

    b.installArtifact(lib);
}
