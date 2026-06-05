const std = @import("std");
const LPVOID = std.os.windows.LPVOID;
const LPCSTR = std.os.windows.LPCSTR;
const LPCWSTR = std.os.windows.LPCWSTR;

pub const Error = error{
    /// Unknown error. Should not be returned.
    Unknown,
    /// MinHook is already initialized.
    AlreadyInitialized,
    /// MinHook is not initialized yet, or already uninitialized.
    NotInitialized,
    /// The hook for the specified target function is already created.
    AlreadyCreated,
    /// The hook for the specified target function is not created yet.
    NotCreated,
    /// The hook for the specified target function is already enabled.
    Enabled,
    /// The hook for the specified target function is not enabled yet, or
    /// already disabled.
    Disabled,
    /// The specified pointer is invalid. It points the address of non-allocated
    /// and/or non-executable region.
    NotExecutable,
    /// The specified target function cannot be hooked.
    UnsupportedFunction,
    /// Failed to allocate memory.
    MemoryAlloc,
    /// Failed to change the memory protection.
    MemoryProtect,
    /// The specified module is not loaded.
    ModuleNotFound,
    /// The specified function is not found.
    FunctionNotFound,
};

const Status = enum(c_int) {
    unknown = -1,
    ok = 0,
    already_initialized = 1,
    not_initialized = 2,
    already_created = 3,
    not_created = 4,
    enabled = 5,
    disabled = 6,
    not_executable = 7,
    unsupported_function = 8,
    memory_alloc = 9,
    memory_protect = 10,
    module_not_found = 11,
    function_not_found = 12,

    fn toError(self: @This()) Error!void {
        return switch (self) {
            .ok => {},
            .unknown => error.Unknown,
            .already_initialized => error.AlreadyInitialized,
            .not_initialized => error.NotInitialized,
            .already_created => error.AlreadyCreated,
            .not_created => error.NotCreated,
            .enabled => error.Enabled,
            .disabled => error.Disabled,
            .not_executable => error.NotExecutable,
            .unsupported_function => error.UnsupportedFunction,
            .memory_alloc => error.MemoryAlloc,
            .memory_protect => error.MemoryProtect,
            .module_not_found => error.ModuleNotFound,
            .function_not_found => error.FunctionNotFound,
        };
    }
};

extern fn MH_Initialize() callconv(.winapi) Status;
extern fn MH_Uninitialize() callconv(.winapi) Status;
extern fn MH_CreateHook(target: LPVOID, detour: LPVOID, original: *LPVOID) callconv(.winapi) Status;
extern fn MH_CreateHookApi(module: LPCWSTR, procName: LPCSTR, detour: LPVOID, original: *LPVOID) callconv(.winapi) Status;
extern fn MH_CreateHookApiEx(module: LPCWSTR, procName: LPCSTR, detour: LPVOID, original: *LPVOID, target: *LPVOID) callconv(.winapi) Status;
extern fn MH_RemoveHook(target: LPVOID) callconv(.winapi) Status;
extern fn MH_EnableHook(target: ?LPVOID) callconv(.winapi) Status;
extern fn MH_DisableHook(target: ?LPVOID) callconv(.winapi) Status;
extern fn MH_QueueEnableHook(target: ?LPVOID) callconv(.winapi) Status;
extern fn MH_QueueDisableHook(target: ?LPVOID) callconv(.winapi) Status;
extern fn MH_ApplyQueued() callconv(.winapi) Status;
// extern fn MH_StatusToString(status: Status) callconv(.winapi) [*:0]const u8;

pub inline fn initialize() Error!void {
    return MH_Initialize().toError();
}

pub inline fn uninitialize() Error!void {
    return MH_Uninitialize().toError();
}

inline fn fnAddr(target: anytype) *anyopaque {
    const info = @typeInfo(@TypeOf(target));
    if (info != .pointer or @typeInfo(info.pointer.child) != .@"fn") {
        @compileError("expected a function pointer, got " ++ @typeName(@TypeOf(target)));
    }
    return @ptrCast(@constCast(target));
}

pub inline fn createHook(target: anytype, detour: @TypeOf(target), trampoline: *@TypeOf(target)) Error!void {
    return MH_CreateHook(fnAddr(target), fnAddr(detour), @ptrCast(trampoline)).toError();
}

pub inline fn createHookApi(module: LPCWSTR, proc_name: LPCSTR, detour: anytype, trampoline: *@TypeOf(detour)) Error!void {
    return MH_CreateHookApi(module, proc_name, fnAddr(detour), @ptrCast(trampoline)).toError();
}

pub inline fn createHookApiEx(module: LPCWSTR, proc_name: LPCSTR, detour: anytype, trampoline: *@TypeOf(detour), target: *@TypeOf(detour)) Error!void {
    return MH_CreateHookApiEx(module, proc_name, fnAddr(detour), @ptrCast(trampoline), @ptrCast(target)).toError();
}

pub inline fn removeHook(target: anytype) Error!void {
    return MH_RemoveHook(fnAddr(target)).toError();
}

pub inline fn enableHook(target: anytype) Error!void {
    return MH_EnableHook(fnAddr(target)).toError();
}

pub inline fn disableHook(target: anytype) Error!void {
    return MH_DisableHook(fnAddr(target)).toError();
}

pub inline fn enableAll() Error!void {
    return MH_EnableHook(null).toError();
}

pub inline fn disableAll() Error!void {
    return MH_DisableHook(null).toError();
}

pub inline fn queueEnableHook(target: anytype) Error!void {
    return MH_QueueEnableHook(fnAddr(target)).toError();
}

pub inline fn queueDisableHook(target: anytype) Error!void {
    return MH_QueueDisableHook(fnAddr(target)).toError();
}

pub inline fn applyQueued() Error!void {
    return MH_ApplyQueued().toError();
}
