# codex-cu MCP

## 架构与背景

`codex-cu MCP` 通过以下机制实现独立桥接：
1. **签名进程隔离（Shim）**：使用 OpenAI 签名的 Node 二进制 (`/Applications/ChatGPT.app/Contents/Resources/cua_node/bin/node`) 作为直接父进程，通过轻量级 `assets/shim.mjs` 启动 `SkyComputerUseClient mcp`。
2. **责任归属解耦（Disclaimed Spawn）**：在 macOS 上调用 `posix_spawnattr_init` 与 `responsibility_spawnattrs_setdisclaim`，放弃父进程责任归属，确保不受调用端进程签名影响。
3. **MCP Stdio 直通与 Elicitation 审批**：双向透传 JSON-RPC 消息帧；在客户端能力中补齐 `elicitation` 声明，对原生 CU 的应用权限请求进行处理（支持自动放行 `CODEX_CU_APPROVAL=auto` 或转发给 MCP 客户端）。
4. **键盘打字保护**：拦截包含非 ASCII 字符的 `type_text` 请求并提供明确指引，避免系统级静默丢失字符。
5. **实时画中画悬浮窗（PiP Window）**：对标 Codex 原生画中画体验，操作后台应用时在屏幕右上角自动显示无感置顶的实时监控小窗。

## 图形化实时悬浮预览小窗口（PiP Window）

在执行 Computer Use 操作后台应用时，屏幕右上角会自动弹出一个原生的实时画中画悬浮小窗口（`codex-cu-pip`）：
- **实时画面呈现**：利用 macOS 原生 ScreenCaptureKit (~8-10 fps) 及 CU 动作返回的实时截图，低延迟同步后台目标应用窗口。
- **无感浮动与不夺焦点**：采用 macOS Accessory 辅助进程策略及 `NSWindow.Level.floating`，小窗口显示和点击交互均不会抢夺用户当前正在输入或操作的窗口焦点。
- **快捷交互**：
  - 点击右上角「↗（切到该应用）」：将目标应用前置并激活。
  - 点击「✕（关闭）」：关闭本次会话的悬浮窗。
  - 标题栏支持任意拖动改变位置。
- **智能生命周期**：
  - 仅在目标应用处于后台时显示；当用户切到该应用时自动隐藏。
  - 超过 20 秒无新动作（空闲超时）自动隐藏；有新动作自动恢复。
  - MCP 服务退出时，悬浮窗进程自动安全退出。
- **配置开关**：
  - 设置环境变量 `CODEX_CU_PIP=false` 或 `0` 可禁用悬浮小窗。
## 构建与运行

```bash
cargo build --release
./target/release/codex-cu-mcp --check-runtime
```

## MCP 配置

在 MCP 配置文件（如 `claude_desktop_config.json` 或各 Agent 配置）中添加：

```json
{
  "mcpServers": {
    "codex-cu": {
      "command": "/<YOUR_PATH>/target/release/codex-cu-mcp",
      "env": {
        "CODEX_CU_APPROVAL": "auto"
      }
    }
  }
}
```

暴露工具列表：
- `list_apps`: 列出当前可操作的应用
- `get_app_state`: 获取应用的可访问性树及元素索引
- `click`: 点击指定元素索引或坐标
- `type_text`: 输入 ASCII 文本
- `press_key`: 发送特定按键或快捷键（如 Return, Tab）
- `set_value`: 直接设置文本框数值（支持全字符集包括中文）
- `scroll`: 滚动
- `drag`: 拖拽
- `select_text`: 选择文本
- `perform_secondary_action`: 执行次要操作
