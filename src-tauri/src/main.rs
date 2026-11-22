// release 构建下切到 windows 子系统，否则 Windows 会为进程附一个控制台窗口。
// 保留 debug_assertions 条件：dev 构建仍然要有终端来看 stdout / panic。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tsubasa_redis_manager_lib::run();
}
