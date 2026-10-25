# 概述
- 代码中不能遗留未使用警告、死码等
- rust不能在生产环境候用unwrap expect 等会造成panic的
- 核心功能必须覆盖功能测试、边界测试
- 客户端语言采用简体中文+英文
- 所有的后端错误采用统一的结构，传递展示给用户的必须是最接近真相的原始错误信息，不能层层叠加：`请求文件出错：授权出错：您没有权限访问所在目录，管理员授权后访客访问`，这里应该直接展示 `您没有权限...`，记录到日志的错误信息应该包含完整的错误信息，如：原始错误、错误码、状态信息等能获取到的，比如http错误应该记录状态码+错误信息
- mod.rs 仅用于定义模块及再导出，不得放实际业务
- traits.rs 用于定义特征
- types.rs 用于定于类型、常量、enum等

# 错误处理规范

## 统一错误结构

所有 Tauri 命令返回 `Result<T, AppError>`。`AppError` 必须可序列化，前端拿到的是结构化对象，**禁止只返回字符串**。结构如下：

- `code`: string —— 稳定的错误码（如 `AUTH_INVALID_TOKEN`、`FS_PERMISSION_DENIED`），供前端分支处理，不随文案变化。
- `message`: string —— 面向用户的一句话，说明“什么操作失败 + 原始原因”，只保留一层上下文。
- `detail`: string | null —— 原始错误信息（底层 errno、库返回的 message），供日志和问题排查；前端默认不展示，可在“详情”中展开。
- `context`: object | null —— 结构化上下文，如 `{ path, operation, peer }`，不拼进 message。

示例 message：`读取文件 /data/a.txt 失败：Permission denied (os error 13)`。
反例（禁止）：`请求文件出错：授权出错：读取失败：Permission denied`。

## 传播规则

- Rust 侧用 `thiserror` 定义领域错误，转换用 `#[from]` / `#[source]` 保留因果链，**禁止用 `map_err(|e| format!("...: {}", e))` 拼接字符串**。
- 只在 Tauri 命令边界做一次“领域错误 → AppError”的映射，内部函数之间直接传播领域错误。
- `message` 只加一层操作上下文（哪个操作、哪个对象），底层原因放进 `detail`，不重复堆叠。

## 用户可见 vs 日志

- 用户可见：`message` + `code`；`detail` 仅在用户主动展开“详情”时展示。
- 日志：记录完整因果链（`source` 链）、`detail`、`context`，便于排查。
- 禁止把内部路径、堆栈、密钥等敏感信息放进 `message`。

## 禁止事项

- 禁止返回裸字符串错误。
- 禁止多层 `format!` 拼接错误。
- 禁止吞掉原始错误（必须保留在 `detail` 或 `source` 链中）。

## 前端错误消费约定

- 前端统一使用一个 `invoke` 包装函数（如 `src/lib/invoke.ts`），所有命令调用必须经过它，禁止在组件里直接裸调 `invoke`。
- 包装函数捕获 reject 后，按 `code` 分支处理；`message` 直接用于 toast / 提示；`detail` 写入 console 或“详情”弹窗，默认不展示给用户。
- 前端不得对 `message` 做字符串匹配来判断错误类型，必须依据 `code`。
- 新增错误码时，前端维护一份 `code → 处理策略` 的映射表，未识别的 `code` 走兜底提示（展示 `message`）。

## 异步/后台任务的错误出口

服务端 `tokio::spawn` 任务、UDP 监听循环、后台同步任务等不经过命令返回值的错误，必须走统一出口，禁止静默丢失：

- **写日志**：记录完整因果链、`detail`、`context`。
- **推送前端**：通过 Tauri event 发送同样的 `AppError` 结构（事件名统一，如 `app://error`），前端用同一个错误处理逻辑消费。
- **任务生命周期**：`tokio::spawn` 返回的 `JoinHandle` 需要被持有或监控；任务 panic 或异常退出时，也要通过上述出口上报，而不是只留在控制台。
- 禁止在后台任务里 `let _ = ...` 吞掉 `Result`。

## 实现骨架（参考）

```rust
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("读取文件 {path} 失败")]
    ReadFile { path: String, #[source] source: std::io::Error },
    #[error("授权失败")]
    Auth(#[from] AuthError),
    // ...
}

#[derive(Debug, serde::Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub context: Option<serde_json::Value>,
}

impl From<DomainError> for AppError {
    fn from(e: DomainError) -> Self {
        // 只在此处做一次映射：code 稳定、message 一层上下文、detail 存 source
    }
}

// 命令边界
#[tauri::command]
async fn read_file(path: String) -> Result<String, AppError> {
    inner_read(&path).await.map_err(AppError::from)
}
```

> 要点：`DomainError` 用 `#[source]` 保留因果链；`AppError` 的构造只在 `From<DomainError>` 一处完成；命令函数体内不做字符串拼接。
