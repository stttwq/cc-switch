# 使用第三方 API 时保留 Codex 远程操作和官方插件：CC Switch 配置攻略

> 适用版本：CC Switch v3.16.1 及以上。本文按当前代码与用户手册整理，不含真实 Access Token 或 API Key。

## 这篇攻略解决什么问题

很多人使用 Codex 时有两个需求：

1. 模型使用 DeepSeek、Kimi、GLM、MiniMax、硅基流动等第三方 API，或者在中转站使用 gpt 模型。
2. 保留 Codex 官方 App 的手机远程操作、官方插件等能力。

之前切换第三方供应商时，旧行为会把第三方 API Key 写进 Codex 的 `auth.json`，从而覆盖原来的官方 ChatGPT / Codex 登录缓存。这样第三方模型能用了，但依赖官方登录态的功能会消失。

现在的行为已经把这个矛盾消掉了：**切换到第三方供应商只写 `config.toml`，不碰 `~/.codex/auth.json`**。于是 Codex App 仍认为你登录的是官方账号，而模型请求走 CC Switch 当前选中的第三方供应商。第三方供应商的 API Key 也不再写进 `config.toml`，而是通过用户级环境变量投递。

## 先看结论

推荐顺序是：

1. 在 CC Switch 的 Codex 面板切换到 `OpenAI Official`。
2. 启动 Codex，并用官方 ChatGPT / Codex 账号登录一次，Free 订阅也可以。
3. 回到 CC Switch，添加或切换到第三方 Codex 供应商。
4. 重新打开终端（新环境变量只有新进程才读得到），再启动 Codex，让 `config.toml` 和模型目录重新加载。

> ℹ️ 不再需要任何「Codex 应用增强」开关，也不需要开启本地路由或接管——本地路由整族能力已移除。

## 准备工作

你需要准备：

- CC Switch v3.16.1 或更新版本。
- 已安装并能启动的 Codex（建议 app 和 cli 都安装）。
- 一个可以登录 Codex 的官方 ChatGPT / Codex 账号，Free 订阅即可。
- 一个第三方 API Key，例如 DeepSeek、Kimi、GLM、MiniMax、OpenRouter、硅基流动等。

请不要手动复制或分享 `~/.codex/auth.json` 的内容。里面保存的是官方登录缓存和 Access Token，属于敏感信息。CC Switch 读取它时也只把内容用于识别登录态，不会把它同步或导出。

## 第一步：先切回 OpenAI Official 并完成官方登录

打开 CC Switch，切到顶部的 `Codex` 标签页。先选择 `OpenAI Official` 供应商（如果没有的话，就在预设供应商当中添加一个），并把它设为当前供应商。

接着启动 Codex（建议启动 cli），按 Codex 的官方登录流程登录你的 ChatGPT / Codex 账号。这个账号可以是 Free 订阅；在这个方案里，它主要负责保留 Codex 官方 App 需要识别的登录身份，不负责第三方模型的计费。

登录完成后，Codex 会在 `~/.codex/auth.json` 中保存官方登录缓存。后面的关键点就是：切换到第三方供应商不会覆盖这个文件。

## 第二步：添加第三方 Codex 供应商

回到 Codex 面板，点击右上角的加号添加供应商。推荐优先使用内置预设，例如 DeepSeek、Kimi、MiniMax、GLM、SiliconFlow 等。

以 DeepSeek 为例，选择预设后只需要填 API Key。预设会自动配置 base URL 与默认模型列表。

> ⚠️ **重要变化**：CC Switch 不再转换请求协议。第三方供应商必须自身提供 Codex 能直接对话的端点（`wire_api = "responses"`）。仅提供 OpenAI Chat Completions 端点的供应商，需要由供应商侧提供 Responses 兼容入口，否则无法通过 Codex 使用——早期版本靠本地路由做协议转换，该能力已移除。

## 第三步：切换第三方供应商并重开终端

回到 Codex 供应商列表，启用你刚添加的第三方供应商。切换完成后：

1. **关闭并重新打开终端** —— 第三方供应商的密钥写在用户级环境变量 `CC_SWITCH_CODEX_API_KEY` 里，运行中的终端读不到新值
2. 重启 Codex —— Codex 在启动时读取 `config.toml`，`/model` 菜单通常也要重启后才会重新加载模型目录

重开后，你可以做一个简单验证：

- 在 Codex App 里，账号信息仍然显示官方账号，这是预期行为。
- 在 CC Switch 里，当前 Codex 供应商显示为第三方供应商。
- 第三方供应商后台或余额记录会出现实际模型请求。

## 背后的原理

Codex 的配置主要分成两个文件：

```text
~/.codex/auth.json
~/.codex/config.toml
```

这两个文件承担的职责不同：

- `auth.json` 保存官方 ChatGPT / Codex 登录缓存，也就是 Codex App 识别官方账号、远程操作和官方插件所需的登录材料。
- `config.toml` 保存当前模型供应商、base URL、模型与模型目录等运行配置。

切换到第三方供应商时，CC Switch 只写 `config.toml`：

```toml
model_provider = "custom"

[model_providers.custom]
name = "DeepSeek"
base_url = "https://api.deepseek.com"
wire_api = "responses"
env_key = "CC_SWITCH_CODEX_API_KEY"
```

`env_key` 是**变量名**而不是密钥值；真实 API Key 存在 Windows 凭据管理器，并在切换时写入用户级环境变量。同时 `auth.json` 保持官方登录缓存不变。于是 Codex App 侧依然能识别官方账号；而模型请求会根据 `config.toml` 的当前 provider 和 base URL 直连第三方 API。

> 💡 `base_url` 是这套方案里唯一的例外：Codex CLI 不支持在该字段做变量引用，所以当前激活供应商的地址会明文写在 `config.toml` 里，其余供应商的地址仍只在凭据管理器中。

还有一道安全门：如果某个 `[model_providers.*]` 表带着 `requires_openai_auth = true` 却没有自己的凭据（既没有 `env_key` 也没有 token），CC Switch 会**拒绝写入**——因为那会让 Codex 回退用 `auth.json` 里的官方登录凭据去访问第三方地址，把官方凭据泄露出去。请为该供应商补全 API Key，或移除这条回退指令。

## 需要理解的副作用

### Codex 里显示的账号始终是官方账号

这是最容易误解的一点。Codex App 看到的是 `auth.json` 里的官方登录态，所以它会继续显示官方账号信息。

但这不代表模型请求还在走官方 OpenAI。实际流量以 CC Switch 当前 Codex 供应商和 `config.toml` 为准。

### 不要用 Codex 账号信息判断计费方

如果你切到 DeepSeek，Codex 里仍然显示官方账号，但模型请求会走 DeepSeek API。计费、限额、错误码和数据策略都应按第三方供应商理解。

### 修改模型映射后要重启 Codex

Codex 的模型目录是启动时读取的。即使 CC Switch 已经生成了新的模型目录，正在运行的 Codex 也不一定会热加载，所以修改模型映射后请重启 Codex。

## 常见问题

**我已经切到第三方 API，为什么 Codex 还显示官方账号？**

这是预期行为。官方账号信息来自 `auth.json`，模型请求的实际供应商来自 `config.toml` 和 CC Switch 当前供应商。

**Free 订阅真的可以吗？**

可以。这里的官方账号主要用于获取并保留 Codex App 需要的官方登录态。第三方模型请求使用的是你在 CC Switch 里配置的第三方 API Key。

**开启后官方插件或手机远程操作还是不可用怎么办？**

先切回 `OpenAI Official`，重新启动 Codex 并完成一次官方登录，然后再切回第三方供应商。切换本身不会覆盖 `auth.json`，所以登录态通常不需要重做。

**第三方请求 404、模型列表不对或流式响应异常怎么办？**

先确认该供应商自身提供 Responses 兼容端点（CC Switch 已不做协议转换），再核对 `config.toml` 里的 `base_url` 与 `model_provider` 指向同一张表，最后确认你重开了终端（`CC_SWITCH_CODEX_API_KEY` 需要新进程才可见）。仍异常时，参考 [5.4 环境变量冲突](../user-manual/zh/5-faq/5.4-env-conflict.md)。

**可以在第三方模式下切回 OpenAI Official 吗？**

可以。切换是普通操作，没有旧的「接管会阻止切官方」的限制——那套本地路由已经移除。切回官方后请同样重开终端。

## 参考链接

- [Codex 桌面应用里看不到自定义模型？（常见问题）](./codex-desktop-custom-model-visibility-zh.md)
- [添加供应商](../user-manual/zh/2-providers/2.1-add.md)
- [切换供应商](../user-manual/zh/2-providers/2.2-switch.md)
- [配置文件说明](../user-manual/zh/5-faq/5.1-config-files.md)
