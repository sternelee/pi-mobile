#ifndef PI_BUN_H
#define PI_BUN_H
/*
 * libpi-bun C ABI — v1（JSON 消息通道）
 *
 * 三方镜像（必须一致，见 docs/CONTRACTS.md §2 与 docs/LIBPI-BUN-NOTES.md）：
 *   - 本头文件（Rust 侧 FFI 声明 + 链接期符号守卫的期望集）
 *   - src-tauri/src/pi_bun/ffi.rs（Rust extern "C" 声明）
 *   - pi-bundle/bridge.ts（JS 侧 hostcall 绑定，经 JSC C API 注册）
 *
 * 线程模型（复刻 skal_entry.zig）：VM 跑在专用 worker 线程；
 * 宿主只允许调用 pibun_post_event / pibun_wake 投递并唤醒，
 * 回调在 worker 线程触发——宿主回调内必须无锁快速返回。
 *
 * 状态: DRAFT（M1 PoC 目标；符号集一旦上真机即冻结为 v1）
 */

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int64_t pibun_handle_t;

/* 错误码 */
#define PIBUN_OK 0
#define PIBUN_E_INVAL (-1)   /* 参数非法 */
#define PIBUN_E_STATE (-2)   /* 状态机错误（未启动/已销毁） */
#define PIBUN_E_VM (-3)      /* VM 内部错误（JS 异常等） */

/*
 * 宿主端口：bun → Rust 的 hostcall 通道。
 * json 为一条完整消息（见 docs/CONTRACTS.md §2.2 hostcall schema）。
 * 返回值：宿主应答的 JSON 字符串（调用方保证 NUL 结尾），
 * 返回 NULL 表示无应答。回调在 worker 线程执行。
 */
typedef const char *(*pibun_host_port_t)(const char *json, void *user_data);

/* 生命周期 */

pibun_handle_t pibun_create_runtime(const char *bundle_path,
                                    const char *home_dir,
                                    const char *tmp_dir,
                                    pibun_host_port_t host_port,
                                    void *host_port_user_data);

int32_t pibun_start(pibun_handle_t rt);   /* 加载并执行 bundle 入口 */
int32_t pibun_stop(pibun_handle_t rt);
void pibun_destroy(pibun_handle_t rt);

/* Rust → bun：投递 UI 指令/审批结果（CONTRACTS §2.3 事件 schema） */
int32_t pibun_post_event(pibun_handle_t rt, const char *json);
void pibun_wake(pibun_handle_t rt);       /* 唤醒 worker 处理队列 */

/* 诊断 */
const char *pibun_version(void);          /* bun/JSC 版本串（.jsc 耦合校验用） */
int64_t pibun_last_activity_ms(pibun_handle_t rt);

#ifdef __cplusplus
}
#endif

#endif /* PI_BUN_H */
