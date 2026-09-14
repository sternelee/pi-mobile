// spike-harness.c — dlopen 宿主 libskal.dylib 并跑 D14 前置探测。
//
// 复现：
//   cc -o /tmp/spike-harness /tmp/spike-harness.c && \
//     /tmp/spike-harness build/skal-macos-spike/libskal.dylib
//
// 观测点（对应 D14 的两个前置 + 父要求的超时回传形态）：
//   [baseline]  skal_evaluate("1+1")            —— 现有 agent 路径没被破坏
//   [Q1]        pibun_spike_second_vm()         —— 第二 VM 能否 init + eval
//   [Q1-iso]    第二 VM 里 agent 全局是否不可达
//   [Q2]        pibun_spike_time_limit(ms)      —— 第二 VM 上死循环是否被终止
//   [Q2-main]   主 VM 设时限后 skal_evaluate 死循环：
//               out_is_error / out_result 的**原始值**（决定结构化错误的包法）
//   [Q2-recov]  clear 之后主 VM 能否继续 eval
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

typedef int64_t (*create_fn)(const char *, size_t);
typedef void (*eval_fn)(int64_t, const char *, size_t, const char *, size_t,
                        char **, size_t *, int *);
typedef void (*free_fn)(char *);
typedef int32_t (*reused_fn)(void);
typedef int32_t (*spike0_fn)(void);
typedef int32_t (*spike1_fn)(uint32_t);

static void *sym(void *lib, const char *name) {
    void *p = dlsym(lib, name);
    if (!p) { printf("  !! dlsym(%s) failed: %s\n", name, dlerror()); exit(2); }
    return p;
}

int main(int argc, char **argv) {
    const char *libpath = argc > 1 ? argv[1] : "libskal.dylib";
    const char *datadir = "/tmp/pi-spike-data";
    mkdir(datadir, 0755);

    printf("== dlopen %s\n", libpath);
    void *lib = dlopen(libpath, RTLD_NOW | RTLD_LOCAL);
    if (!lib) { printf("!! dlopen failed: %s\n", dlerror()); return 1; }

    create_fn create = (create_fn)sym(lib, "skal_create_runtime");
    eval_fn eval = (eval_fn)sym(lib, "skal_evaluate");
    free_fn freestr = (free_fn)sym(lib, "skal_free_string");
    reused_fn reused = (reused_fn)sym(lib, "skal_runtime_was_reused");
    spike0_fn spike_second_vm = (spike0_fn)sym(lib, "pibun_spike_second_vm");
    spike1_fn spike_time_limit = (spike1_fn)sym(lib, "pibun_spike_time_limit");
    spike1_fn spike_main_limit = (spike1_fn)sym(lib, "pibun_spike_main_time_limit");
    spike0_fn spike_main_clear = (spike0_fn)sym(lib, "pibun_spike_main_clear_limit");

    printf("== skal_create_runtime(%s)\n", datadir);
    int64_t h = create(datadir, strlen(datadir));
    if (h == 0) { printf("!! create_runtime returned 0\n"); return 1; }
    printf("   handle=%lld reused=%d\n", (long long)h, reused());

    // 统一求值助手：打印 out_is_error 与 out_result 的原始内容
    #define EVAL(label, src) do {                                                  \
        char *r = NULL; size_t rlen = 0; int iserr = 0;                            \
        const char *s = (src); size_t slen = strlen(s); const char *url = label;   \
        eval(h, s, slen, url, strlen(url), &r, &rlen, &iserr);                     \
        printf("   [%s] out_is_error=%d out_len=%zu out_result=\"%.200s\"\n",      \
               label, iserr, rlen, r ? r : "(null)");                              \
        if (r) freestr(r);                                                         \
    } while (0)

    printf("== [baseline] 现有路径：skal_evaluate(\"1+1\")\n");
    EVAL("baseline", "1+1");

    printf("== [Q1] 第二 VM（pibun_spike_second_vm）\n");
    int q1 = spike_second_vm();
    printf("   -> q1=%d  (1=第二 VM init+eval 成功)\n", q1);

    printf("== [Q2] 第二 VM 上的执行时限（pibun_spike_time_limit(1500)）\n");
    int q2 = spike_time_limit(1500);
    printf("   -> q2=%d  (1=死循环被终止)\n", q2);

    printf("== [Q2-main] 主 VM 设 1500ms 时限后 skal_evaluate(\"while(true){}\")\n");
    printf("   注意：此后主 VM 在不 clear 的情况下都受时限约束\n");
    spike_main_limit(1500);
    EVAL("q2-main-deadloop", "while (true) {} 'unreachable'");

    printf("== [Q2-recov] clear 时限后主 VM 能否继续跑\n");
    spike_main_clear();
    EVAL("q2-recovery", "'recovered:' + (1+1)");

    printf("== 存活确认：进程未崩（走到这里即未崩）\n");
    dlclose(lib);
    return 0;
}
