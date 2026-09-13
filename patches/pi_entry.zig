//! pi_entry.zig — libpi-bun：嵌入式 bun + JSC 运行时，`pi_bun_*` C ABI。
//!
//! 派生自 skal 的 skal_entry.zig（Apache-2.0，© skal-multiplatform），
//! 按 pi-mobile 需求裁剪：VM 生命周期、同步求值、hostcall 端口、事件队列。
//! 不含：6MiB 零拷贝桥、原生 KV store、Dart doorbell、Solid 渲染支持。
//!
//! 线程模型（与 skal 相同）：
//!   ┌─────────────┐  enqueueTaskConcurrent   ┌──────────────────────┐
//!   │ 宿主线程(Rust)│ ───────────────────────► │ pi worker 线程 owns VM│
//!   │             │ ◄──── host_port 回调 ──── │  tick 事件循环         │
//!   └─────────────┘                          └──────────────────────┘
//!
//! ABI 镜像（三方必须一致）：src-tauri/pi_bun/include/pi_bun.h、
//! src-tauri/src/pi_bun/ffi.rs、pi-bundle/bridge.ts。
//! 参考：docs/LIBPI-BUN-NOTES.md、docs/CONTRACTS.md §2。

const std = @import("std");
const builtin = @import("builtin");
const bun = @import("bun");
const jsc = bun.jsc;

// ── JSC C API 最小子集（libJavaScriptCore.a 稳定公开 ABI）──────────────

const JSContextRef = *anyopaque;
const JSObjectRef = *anyopaque;
const JSValueRef = *anyopaque;
const JSStringRef = *anyopaque;

const JSObjectCallAsFunctionCallback = *const fn (
    ctx: JSContextRef,
    function: JSObjectRef,
    thisObject: JSObjectRef,
    argumentCount: usize,
    arguments: [*]const JSValueRef,
    exception: ?*?JSValueRef,
) callconv(.c) ?JSValueRef;

extern fn JSStringCreateWithUTF8CString(string: [*:0]const u8) JSStringRef;
extern fn JSStringRelease(string: JSStringRef) void;
extern fn JSContextGetGlobalObject(ctx: JSContextRef) JSObjectRef;
extern fn JSObjectMakeFunctionWithCallback(
    ctx: JSContextRef,
    name: ?JSStringRef,
    callAsFunction: JSObjectCallAsFunctionCallback,
) JSObjectRef;
extern fn JSObjectSetProperty(
    ctx: JSContextRef,
    object: JSObjectRef,
    propertyName: JSStringRef,
    value: JSValueRef,
    attributes: u32,
    exception: ?*?JSValueRef,
) void;
extern fn JSValueMakeString(ctx: JSContextRef, string: JSStringRef) JSValueRef;
extern fn JSValueMakeUndefined(ctx: JSContextRef) JSValueRef;
extern fn JSValueToStringCopy(ctx: JSContextRef, value: JSValueRef, exception: ?*?JSValueRef) ?JSStringRef;
extern fn JSStringGetMaximumUTF8CStringSize(string: JSStringRef) usize;
extern fn JSStringGetUTF8CString(string: JSStringRef, buffer: [*]u8, bufferSize: usize) usize;
extern fn JSObjectGetProperty(ctx: JSContextRef, object: JSObjectRef, propertyName: JSStringRef, exception: ?*?JSValueRef) JSValueRef;
extern fn JSValueIsObject(ctx: JSContextRef, value: JSValueRef) bool;
extern fn JSValueProtect(ctx: JSContextRef, value: JSValueRef) void;
extern fn JSObjectCallAsFunction(
    ctx: JSContextRef,
    function: JSObjectRef,
    thisObject: JSObjectRef,
    argumentCount: usize,
    arguments: [*]const JSValueRef,
    exception: ?*?JSValueRef,
) ?JSValueRef;

/// bun 内部求值入口（同 skal；Program 语义，支持 Promise 等待）
extern fn Bun__REPL__evaluate(
    globalObject: *jsc.JSGlobalObject,
    sourcePtr: [*]const u8,
    sourceLen: usize,
    filenamePtr: [*]const u8,
    filenameLen: usize,
    exception_out: *jsc.JSValue,
) jsc.JSValue;

// ── ABI 常量与类型（镜像 pi_bun.h）───────────────────────────────────

const PIBUN_OK: i32 = 0;
const PIBUN_E_INVAL: i32 = -1;
const PIBUN_E_STATE: i32 = -2;
const PIBUN_E_VM: i32 = -3;

/// bun → Rust hostcall：json 一条完整消息；应答为宿主侧缓冲的 NUL 结尾
/// C 字符串（有效期为下一次同线程回调前）。在 VM worker 线程上调用。
const HostPortFn = *const fn (
    json: [*]const u8,
    len: usize,
    user_data: ?*anyopaque,
) callconv(.c) ?[*:0]const u8;

const HostPort = struct {
    func: ?HostPortFn = null,
    user_data: ?*anyopaque = null,
};

// ── Runtime ──────────────────────────────────────────────────────────

const Runtime = struct {
    allocator: std.mem.Allocator,
    vm: *jsc.VirtualMachine = undefined,
    worker_thread: std.Thread = undefined,
    ready: std.Thread.ResetEvent = .{},
    init_failed: std.atomic.Value(bool) = .{ .raw = false },

    /// 宿主端口（create 时一次性设置；worker 启动前写入 → VM 线程只读）
    host_port: HostPort = .{},
    /// bundle 路径（pibun_start 时求值）
    bundle_path: [:0]const u8 = "",
    /// App 数据目录（create 时传入）—— 仅装成 JS 全局，不改环境变量。
    /// 命名对齐 skal 的 `__skal_data_dir`（见 skal_create_runtime 注释）。
    data_dir: []const u8 = "",

    /// Rust → bun 事件队列（pibun_post_event 入队；pump 任务出队并分发）
    event_mutex: std.Thread.Mutex = .{},
    event_queue: std.ArrayListUnmanaged([]u8) = .{},

    /// 缓存的 `__pi_on_event` 引用（首 pump 后 Protect，GC 不回收）
    on_event_fn: ?JSObjectRef = null,

    fn init(allocator: std.mem.Allocator, data_dir: []const u8) !*Runtime {
        const self = try allocator.create(Runtime);
        errdefer allocator.destroy(self);
        self.* = .{ .allocator = allocator };
        if (data_dir.len > 0) {
            self.data_dir = allocator.dupe(u8, data_dir) catch "";
        }
        self.worker_thread = try std.Thread.spawn(.{}, workerMain, .{self});
        self.ready.wait();
        if (self.init_failed.load(.acquire)) {
            return error.RuntimeInitFailed;
        }
        return self;
    }

    /// Worker 线程主函数：终身持有 VM，跑 bun 事件循环直至进程退出。
    fn workerMain(self: *Runtime) void {
        // ── iOS 真机：关 JIT（必须在 bun.jsc.initialize 之前）──────────
        //
        // Apple 不允许第三方 app 拥有可写可执行内存（W^X），JSC 的
        // ExecutableAllocator 拿不到 exec 页。WebKit 提供两个降级点
        // （都在 runtime/VM.cpp 的 enableAssembler）：
        //   1. 读环境变量 `JavaScriptCoreUseJIT`（VM.cpp:206，**经 getenv()
        //      而非 Options**）—— 注意：这条能生效是因为 getenv 读 C
        //      `environ`，而 Zig 的 `std.os.environ` 是启动时快照的切片，
        //      `bun.jsc.initialize` 传给 JSCInitialize 的正是后者 ——
        //      所以只能靠 getenv 这条路径，不能靠 BUN_JSC_* 前缀。
        //   2. `isJITEnabled()`（ExecutableAllocator.cpp:146）检查
        //      dynamic-codesigning / com.apple.developer.cs.allow-jit
        //      entitlement。
        // 只依赖 (2) 不够稳：reservation 为空但 isValid() 可能仍为 true，
        // 分配 exec 页时才失败。显式走 (1) 让 VM::computeCanUseJIT()
        // 直接得出 canUseJIT=false → InitializeThreading 将
        // Options::useJIT() 置 false（并 notifyOptionsChanged 级联关掉
        // useWasm 等依赖项），全程解释器执行。
        //
        // 时序关键：`canUseAssembler()` 用 std::call_once 缓存，而它由
        // JSC::initialize() 内部的 VM::computeCanUseJIT() 触发 —— 两者都
        // 发生在 bun.jsc.initialize() 里。所以 setenv 必须排在它前面。
        // 编译期 JIT 代码仍然构建（DOMJIT/DFG 类型依赖），但不执行 ——
        // 与 bun Android 预构建同一策略。
        if (builtin.os.tag == .ios) {
            _ = setenv("JavaScriptCoreUseJIT", "0", 1);
        }

        bun.jsc.initialize(false);

        // console.* 崩溃底线（skal 教训）：嵌入式路径不初始化
        // Output.Source，console.log 会写穿未初始化的 threadlocal
        // source 而打崩 VM。只调用 setInit（完整 init 会跑
        // bun_initialize_process()，嵌入库禁止触碰宿主 stdio/信号）。
        bun.Output.Source.setInit(
            bun.sys.File.from(std.fs.File.stdout()),
            bun.sys.File.from(std.fs.File.stderr()),
        );

        const args = std.mem.zeroes(bun.schema.api.TransformOptions);
        const vm = jsc.VirtualMachine.init(.{
            .allocator = self.allocator,
            .args = args,
            .smol = true,
            .is_main_thread = true,
        }) catch {
            self.init_failed.store(true, .release);
            self.ready.set();
            return;
        };
        self.vm = vm;

        active_runtime = self;
        installPiGlobals(vm);

        self.ready.set();

        // 长驻事件循环（skal 修正版）：有活时 tick + autoTickActive
        // （setTimeout/IO 才触发）；全空闲时 tickPossiblyForever 阻塞，
        // 由 enqueueTaskConcurrent 的显式唤醒打破。
        while (true) {
            while (vm.isEventLoopAlive()) {
                vm.tick();
                vm.eventLoop().autoTickActive();
            }
            vm.eventLoop().tickPossiblyForever();
        }
    }
};

/// worker 线程在 installPiGlobals 前设置；JSC 宿主回调经它找回 Runtime。
threadlocal var active_runtime: ?*Runtime = null;

/// 全局单例（每进程一个 VM —— JSC is_main_thread 语义）。
var global_runtime: ?*Runtime = null;
var global_mutex: std.Thread.Mutex = .{};

// ── 全局安装（JSC C API）─────────────────────────────────────────────

fn installHostFn(ctx: JSContextRef, global_obj: JSObjectRef, name: [*:0]const u8, cb: JSObjectCallAsFunctionCallback) void {
    const name_str = JSStringCreateWithUTF8CString(name);
    defer JSStringRelease(name_str);
    const fn_obj = JSObjectMakeFunctionWithCallback(ctx, name_str, cb);
    JSObjectSetProperty(ctx, global_obj, name_str, @ptrCast(fn_obj), 0, null);
}

fn installPiGlobals(vm: *jsc.VirtualMachine) void {
    const ctx: JSContextRef = @ptrCast(vm.global);
    const global_obj = JSContextGetGlobalObject(ctx);

    // __pi_hostcall(json) -> 应答 json 字符串（同步，走宿主端口）
    installHostFn(ctx, global_obj, "__pi_hostcall", hostcall_jsCallback);

    // 数据目录作为 JS 全局（skal 同款做法）—— 见 skal_create_runtime
    // 注释：为什么不用 HOME。bundle 实际走 __PI_CONFIG.dataDir（Rust 注入），
    // 这里装两个名字只为诊断（pi-bundle/hello.js 就探 __skal_data_dir）。
    if (active_runtime) |rt| {
        if (rt.data_dir.len > 0) {
            installStringGlobal(ctx, global_obj, "__pi_data_dir", rt.data_dir);
            installStringGlobal(ctx, global_obj, "__skal_data_dir", rt.data_dir);
        }
    }
}

/// 把一个 Zig 字符串装成 JS 全局（内部 makeString + setProperty）。
fn installStringGlobal(ctx: JSContextRef, global_obj: JSObjectRef, name: [*:0]const u8, value: []const u8) void {
    const name_str = JSStringCreateWithUTF8CString(name);
    defer JSStringRelease(name_str);
    const val_buf = std.heap.c_allocator.allocSentinel(u8, value.len, 0) catch return;
    defer std.heap.c_allocator.free(val_buf);
    @memcpy(val_buf[0..value.len], value);
    const val_str = JSStringCreateWithUTF8CString(val_buf.ptr);
    defer JSStringRelease(val_str);
    JSObjectSetProperty(ctx, global_obj, name_str, JSValueMakeString(ctx, val_str), 0, null);
}

/// JS 侧 `__pi_hostcall(json)`：取参 → 宿主端口 → 应答字符串。
fn hostcall_jsCallback(
    ctx: JSContextRef,
    _: JSObjectRef,
    _: JSObjectRef,
    argumentCount: usize,
    arguments: [*]const JSValueRef,
    exception: ?*?JSValueRef,
) callconv(.c) ?JSValueRef {
    const rt = active_runtime orelse return JSValueMakeUndefined(ctx);
    if (argumentCount < 1 or rt.host_port.func == null) {
        return JSValueMakeUndefined(ctx);
    }

    // arg0 → UTF-8
    const str_ref = JSValueToStringCopy(ctx, arguments[0], exception) orelse
        return JSValueMakeUndefined(ctx);
    const max = JSStringGetMaximumUTF8CStringSize(str_ref);
    const buf = rt.allocator.alloc(u8, max) catch {
        JSStringRelease(str_ref);
        return JSValueMakeUndefined(ctx);
    };
    defer rt.allocator.free(buf);
    const written = JSStringGetUTF8CString(str_ref, buf.ptr, max); // 含 NUL
    JSStringRelease(str_ref);
    if (written == 0) return JSValueMakeUndefined(ctx);

    // 宿主端口同步回调（VM worker 线程上执行；宿主须快速返回）
    const resp = rt.host_port.func.?(buf.ptr, written - 1, rt.host_port.user_data) orelse
        return JSValueMakeUndefined(ctx);

    const resp_str = JSStringCreateWithUTF8CString(resp);
    defer JSStringRelease(resp_str);
    return JSValueMakeString(ctx, resp_str);
}

// ── 求值任务（宿主线程 → VM 线程）────────────────────────────────────

const EvalRequest = struct {
    rt: *Runtime,
    source: []const u8,
    url: []const u8,
    /// null = fire-and-forget（bundle 启动）；非 null = 同步等待结果
    reply: ?*SyncReply = null,

    any: bun.jsc.AnyTask = undefined,
    concurrent: bun.jsc.ConcurrentTask = undefined,

    fn runOnWorker(self: *EvalRequest) void {
        self.any = bun.jsc.AnyTask.New(EvalRequest, runOnVmThread).init(self);
        self.concurrent = .{ .task = self.any.task(), .next = .none };
        trace("enqueue start", .{});
        self.rt.vm.eventLoop().enqueueTaskConcurrent(&self.concurrent);
        trace("enqueue done, waiting", .{});
        if (self.reply) |reply| reply.done.wait();
        trace("wait returned", .{});
    }

    fn runOnVmThread(self: *EvalRequest) bun.JSError!void {
        trace("vm-thread run enter", .{});
        const global = self.rt.vm.global;
        var exception: jsc.JSValue = .js_undefined;
        const result = Bun__REPL__evaluate(
            global,
            self.source.ptr,
            self.source.len,
            self.url.ptr,
            self.url.len,
            &exception,
        );

        var final = result;
        var is_error = false;
        if (exception != .js_undefined) {
            is_error = true;
            final = exception;
        } else if (result.asAnyPromise()) |promise| {
            // bundle 入口返回 Promise（agent 循环异步）——等待落定
            self.rt.vm.eventLoop().waitForPromise(promise);
            final = promise.result(global.vm());
            if (promise.status() == .rejected) is_error = true;
        }

        const text = final.toUTF8Bytes(global, self.rt.allocator) catch
            self.rt.allocator.dupe(u8, "<toString failed>") catch return;
        trace("vm-thread evaluated err={} len={d}", .{ is_error, text.len });
        if (self.reply) |reply| {
            reply.complete(text, is_error);
        } else if (is_error) {
            // bundle 启动失败仅记录（agent 循环正常时永不结束）
            std.debug.print("pi-bun: bundle eval failed: {s}\n", .{text});
            self.rt.allocator.free(text);
        } else {
            self.rt.allocator.free(text);
        }
    }
};

const SyncReply = struct {
    result_buf: []u8 = "",
    is_error: bool = false,
    done: std.Thread.ResetEvent = .{},

    /// 完成应答。**必须逐字段写，不能整体赋值 `reply.* = .{...}`** ——
    /// 整体赋值会把 `done` 一并重置成全新的 ResetEvent，而宿主线程此刻
    /// 正阻塞在同一个 `done` 上等待唤醒。在 waiter 等待期间覆写同步原语
    /// 是数据竞争，宿主会永久挂在 wait()（真机实测：第一个
    /// skal_evaluate 永不返回）。所以先写载荷，最后 set。
    fn complete(self: *SyncReply, buf: []u8, is_error: bool) void {
        self.result_buf = buf;
        self.is_error = is_error;
        self.done.set();
    }
};

// ── 事件泵（Rust → JS：__pi_on_event(json)）──────────────────────────

const EventPumpTask = struct {
    rt: *Runtime,
    any: bun.jsc.AnyTask = undefined,
    concurrent: bun.jsc.ConcurrentTask = undefined,

    fn run(self: *EventPumpTask) bun.JSError!void {
        const rt = self.rt;
        defer rt.allocator.destroy(self);

        const global = rt.vm.global;
        const ctx: JSContextRef = @ptrCast(global);

        // 懒解析并 Protect `__pi_on_event`（GC 不回收）
        if (rt.on_event_fn == null) {
            const global_obj = JSContextGetGlobalObject(ctx);
            const name = JSStringCreateWithUTF8CString("__pi_on_event");
            defer JSStringRelease(name);
            const value = JSObjectGetProperty(ctx, global_obj, name, null);
            if (JSValueIsObject(ctx, value)) {
                JSValueProtect(ctx, value);
                rt.on_event_fn = value;
            }
        }
        const fn_obj = rt.on_event_fn orelse {
            // bundle 未跑完即 post —— 事件丢弃；宿主应等 start 完成再 post
            std.debug.print("pi-bun: __pi_on_event not installed, dropping events\n", .{});
            return;
        };

        // 出队全部事件，逐条调用
        var batch: std.ArrayListUnmanaged([]u8) = .{};
        {
            rt.event_mutex.lock();
            defer rt.event_mutex.unlock();
            while (rt.event_queue.items.len > 0) {
                batch.append(rt.allocator, rt.event_queue.orderedRemove(0)) catch break;
            }
        }
        defer {
            for (batch.items) |item| rt.allocator.free(item);
            batch.deinit(rt.allocator);
        }

        const global_obj = JSContextGetGlobalObject(ctx);
        for (batch.items) |json| {
            const arg_str = JSStringCreateWithUTF8CString(@ptrCast(json.ptr));
            defer JSStringRelease(arg_str);
            const arg_val = JSValueMakeString(ctx, arg_str);
            var exception: ?JSValueRef = null;
            _ = JSObjectCallAsFunction(ctx, fn_obj, global_obj, 1, @ptrCast(&arg_val), &exception);
        }
    }
};

fn enqueueEventPump(rt: *Runtime) void {
    const task = rt.allocator.create(EventPumpTask) catch return;
    task.* = .{ .rt = rt };
    task.any = bun.jsc.AnyTask.New(EventPumpTask, EventPumpTask.run).init(task);
    task.concurrent = .{ .task = task.any.task(), .next = .none };
    rt.vm.eventLoop().enqueueTaskConcurrent(&task.concurrent);
}

// ── 导出函数（pi_bun_* ABI，镜像 pi_bun.h）───────────────────────────

export fn pibun_create_runtime(
    bundle_path: ?[*:0]const u8,
    home_dir: ?[*:0]const u8,
    tmp_dir: ?[*:0]const u8,
    host_port: ?HostPortFn,
    user_data: ?*anyopaque,
) callconv(.c) i64 {
    _ = home_dir;
    _ = tmp_dir; // 环境由宿主在 create 前 setenv（HOME/TMPDIR → app_data 子目录）

    global_mutex.lock();
    defer global_mutex.unlock();
    if (global_runtime) |rt| {
        // 每进程一个 VM：复用既有句柄（skal 同款语义）
        return @intCast(@intFromPtr(rt));
    }

    const allocator = std.heap.c_allocator;
    // pibun_create_runtime 的 home_dir 参数是遗留的（pi_bun.h v1 草案）；
    // 数据目录一律走 skal_create_runtime(dir,len)（Rust 侧实际用的入口）。
    const rt = Runtime.init(allocator, "") catch {
        std.debug.print("pi-bun: VM init failed\n", .{});
        return 0;
    };
    rt.host_port = .{ .func = host_port, .user_data = user_data };
    if (bundle_path) |p| {
        rt.bundle_path = allocator.dupeZ(u8, std.mem.span(p)) catch "";
    }
    global_runtime = rt;
    return @intCast(@intFromPtr(rt));
}

export fn pibun_start(rt_handle: i64) callconv(.c) i32 {
    global_mutex.lock();
    const rt_opt = global_runtime;
    global_mutex.unlock();
    const rt = rt_opt orelse return PIBUN_E_STATE;
    if (@intFromPtr(rt) != rt_handle) return PIBUN_E_INVAL;
    if (rt.bundle_path.len == 0) return PIBUN_E_INVAL;

    const source = std.fs.cwd().readFileAlloc(rt.allocator, rt.bundle_path, 256 * 1024 * 1024) catch |e| {
        std.debug.print("pi-bun: read bundle failed: {}\n", .{e});
        return PIBUN_E_VM;
    };

    const req = rt.allocator.create(EvalRequest) catch return PIBUN_E_VM;
    req.* = .{ .rt = rt, .source = source, .url = "pi-bundle/entry.js", .reply = null };
    req.runOnWorker(); // fire-and-forget：agent 循环常驻
    return PIBUN_OK;
}

export fn pibun_stop(rt_handle: i64) callconv(.c) i32 {
    _ = rt_handle;
    // bun VM 无进程级 teardown（skal 同款：故意泄漏，契约上句柄作废）
    return PIBUN_OK;
}

export fn pibun_destroy(rt_handle: i64) callconv(.c) void {
    _ = rt_handle;
}

export fn pibun_post_event(rt_handle: i64, json: [*]const u8, len: usize) callconv(.c) i32 {
    global_mutex.lock();
    const rt_opt = global_runtime;
    global_mutex.unlock();
    const rt = rt_opt orelse return PIBUN_E_STATE;
    if (@intFromPtr(rt) != rt_handle) return PIBUN_E_INVAL;

    const copy = rt.allocator.alloc(u8, len) catch return PIBUN_E_VM;
    @memcpy(copy, json[0..len]);
    rt.event_mutex.lock();
    rt.event_queue.append(rt.allocator, copy) catch {
        rt.event_mutex.unlock();
        rt.allocator.free(copy);
        return PIBUN_E_VM;
    };
    rt.event_mutex.unlock();
    return PIBUN_OK;
}

export fn pibun_wake(rt_handle: i64) callconv(.c) void {
    global_mutex.lock();
    const rt_opt = global_runtime;
    global_mutex.unlock();
    if (rt_opt == null) return;
    const rt = rt_opt.?;
    if (@intFromPtr(rt) != rt_handle) return;
    enqueueEventPump(rt);
}

export fn pibun_version() callconv(.c) [*:0]const u8 {
    return "libpi-bun 0.1.0";
}

// ── skal_* 兼容层 ─────────────────────────────────────────────────────
//
// pi-mobile 的 Rust 侧（src-tauri/src/pi_bun/mod.rs）目前绑定的是 skal 预构建
// 的 ABI（skal_create_runtime / skal_evaluate / skal_free_string /
// skal_runtime_was_reused）—— Android 走预构建 libskal.so，iOS 走本文件
// 从源码构建的 libskal.dylib。两边必须导出同一套符号，故这层薄包装把
// pibun_* 机制映射到 skal_* 签名（语义与 skal.h 一致）。
//
// 与 pibun_* 的差异：
//   * create 只有 dir 一个参数（无 host_port；pi-bundle 走 loopback HTTP）
//   * evaluate 结果用 C 分配器 NUL 结尾，由 skal_free_string 释放
//   * 无 start（bundle 由宿主用 skal_evaluate 显式求值）

var global_reused: bool = false;

// libc setenv（本 zig 版本的 std.posix 无此成员）
// 仅用于 iOS 关 JIT（JavaScriptCoreUseJIT），不用于 HOME。
extern "c" fn setenv(name: [*:0]const u8, value: [*:0]const u8, overwrite: c_int) c_int;
extern "c" fn getenv(name: [*:0]const u8) ?[*:0]u8;

/// Zig 侧追踪：追加到 `$HOME/Documents/pi-bun.log`（与 Rust 侧 logcat
/// 同一个文件）。真机上 println/stderr 都拿不到（devicectl 不转 stdout，
/// idevicesyslog 在 CoreDevice 隧道占用 uSMux 后连不上），文件是唯一
/// 可靠通道。只为排障，每次调用开/关文件——不在热路径上。
fn trace(comptime fmt: []const u8, args: anytype) void {
    const home_ptr = getenv("HOME") orelse return;
    const home = std.mem.span(home_ptr);
    var path_buf: [512]u8 = undefined;
    const path = std.fmt.bufPrint(&path_buf, "{s}/Documents/pi-bun.log", .{home}) catch return;
    const f = std.fs.cwd().openFile(path, .{ .mode = .write_only }) catch return;
    defer f.close();
    f.seekFromEnd(0) catch {};
    var line_buf: [640]u8 = undefined;
    const msg = std.fmt.bufPrint(&line_buf, "[pi-zig] " ++ fmt ++ "\n", args) catch return;
    f.writeAll(msg) catch {};
}

/// skal_create_runtime(dir, dir_len) → 句柄（0 = 失败）。
///
/// dir = App 数据目录。**不要把 dir 写进 HOME/TMPDIR** —— Tauri 的
/// `app_data_dir()` = `dirs::data_dir()` + bundle_id，而 iOS 上
/// `dirs::data_dir()` 就是 `$HOME/Library/Application Support`。
/// 一旦把 HOME 改成 dir，第二次 `app_data_dir()` 就会得到
/// `<dir>/Library/Application Support/<bundle-id>`（实测出现的双层嵌套），
/// 两个路径各建一份 sessions/workspace → 会话文件看不到。
///
/// skal 上游的做法：不碰环境变量，只把 dir 装成 JS 全局
/// `globalThis.__skal_data_dir` 供 JS 侧读取。我们的 bundle 走的是
/// `__PI_CONFIG.dataDir`（Rust 在求值 bundle 前注入），所以这里装全局只是
/// 为诊断/兼容；两个名字都装。
export fn skal_create_runtime(dir: [*]const u8, dir_len: usize) callconv(.c) i64 {
    global_mutex.lock();
    defer global_mutex.unlock();
    if (global_runtime) |rt| {
        global_reused = true;
        return @intCast(@intFromPtr(rt));
    }

    const rt = Runtime.init(std.heap.c_allocator, if (dir_len > 0) dir[0..dir_len] else "") catch {
        std.debug.print("pi-bun: VM init failed\n", .{});
        return 0;
    };
    global_runtime = rt;
    global_reused = false;
    return @intCast(@intFromPtr(rt));
}

/// skal_runtime_was_reused() → 1 = 复用了既有 VM（每进程一个）。
export fn skal_runtime_was_reused() callconv(.c) i32 {
    return if (global_reused) 1 else 0;
}

/// skal_evaluate(handle, source, source_len, url, url_len, out, out_len, out_is_error) —— void。
/// 同步求值并等待 Promise 落定；结果由 skal_free_string 释放。
export fn skal_evaluate(
    rt_handle: i64,
    source: [*]const u8,
    source_len: usize,
    url: [*]const u8,
    url_len: usize,
    out_result: *?[*:0]u8,
    out_result_len: *usize,
    out_is_error: *i32,
) callconv(.c) void {
    global_mutex.lock();
    const rt_opt = global_runtime;
    global_mutex.unlock();

    const rt = rt_opt orelse {
        out_result.* = null;
        out_result_len.* = 0;
        out_is_error.* = 1;
        return;
    };
    if (@intFromPtr(rt) != rt_handle) {
        out_result.* = null;
        out_result_len.* = 0;
        out_is_error.* = 1;
        return;
    }

    const reply = rt.allocator.create(SyncReply) catch {
        out_result.* = null;
        out_result_len.* = 0;
        out_is_error.* = 1;
        return;
    };
    reply.* = .{};
    const req = rt.allocator.create(EvalRequest) catch {
        out_result.* = null;
        out_result_len.* = 0;
        out_is_error.* = 1;
        return;
    };
    req.* = .{
        .rt = rt,
        .source = source[0..source_len],
        .url = url[0..url_len],
        .reply = reply,
    };
    trace("eval enter len={d} url={s}", .{ source_len, url[0..@min(url_len, 48)] });
    req.runOnWorker();
    trace("eval back err={} len={d}", .{ reply.is_error, reply.result_buf.len });

    // 复制到 libc 堆 + NUL 结尾（skal_free_string 用 free() 释放）
    const n = reply.result_buf.len;
    const raw = std.c.malloc(n + 1) orelse {
        out_result.* = null;
        out_result_len.* = 0;
        out_is_error.* = 1;
        return;
    };
    const cbuf: [*]u8 = @ptrCast(raw);
    @memcpy(cbuf[0..n], reply.result_buf);
    cbuf[n] = 0;
    out_result.* = @ptrCast(cbuf);
    out_result_len.* = n;
    out_is_error.* = if (reply.is_error) 1 else 0;
}

/// skal_free_string(ptr) —— 释放 skal_evaluate 返回的缓冲区。
export fn skal_free_string(ptr: ?[*:0]u8) callconv(.c) void {
    if (ptr) |p| std.c.free(@ptrCast(p));
}

/// 同步求值（PoC/诊断用，镜像 skal_evaluate 语义）。
export fn pibun_evaluate(
    rt_handle: i64,
    source: [*]const u8,
    source_len: usize,
    url: [*]const u8,
    url_len: usize,
    out_result: *[*]u8,
    out_result_len: *usize,
    out_is_error: *i32,
) callconv(.c) i32 {
    global_mutex.lock();
    const rt_opt = global_runtime;
    global_mutex.unlock();
    const rt = rt_opt orelse return PIBUN_E_STATE;
    if (@intFromPtr(rt) != rt_handle) return PIBUN_E_INVAL;

    const reply = rt.allocator.create(SyncReply) catch return PIBUN_E_VM;
    reply.* = .{};
    const req = rt.allocator.create(EvalRequest) catch return PIBUN_E_VM;
    req.* = .{
        .rt = rt,
        .source = source[0..source_len],
        .url = url[0..url_len],
        .reply = reply,
    };
    req.runOnWorker();

    out_result.* = @constCast(reply.result_buf.ptr);
    out_result_len.* = reply.result_buf.len;
    out_is_error.* = if (reply.is_error) 1 else 0;
    return PIBUN_OK;
}




