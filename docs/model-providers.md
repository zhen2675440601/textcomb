# 模型配置与质量声明

TextComb 支持三种模型服务接口格式。选择的是**接口协议**，不是服务商名称；同一服务商可能同时提供多种协议，必须按实际 API 地址选择。

| 接口格式 | 默认基础地址 | TextComb 请求端点 | 认证方式 |
| --- | --- | --- | --- |
| OpenAI Responses | `https://api.openai.com/v1` | `/responses` | `Authorization: Bearer ...` |
| OpenAI Compatible | `https://api.openai.com/v1` | `/chat/completions` | `Authorization: Bearer ...` |
| Anthropic | `https://api.anthropic.com/v1` | `/messages` | `x-api-key` 与 `anthropic-version: 2023-06-01` |

基础地址可以填写服务商、网关或本地服务的地址。若地址已经以对应端点结尾，TextComb 不会重复追加；否则会按上表追加端点。例如，OpenAI Compatible 的常见完整地址是 `https://example.com/v1/chat/completions`，可填写 `https://example.com/v1` 或该完整地址。

## 创建与修改配置

模型设置页可以创建、测试、启停和修改模型配置。“初检模型”用于第一轮高召回问题发现；“复核模型”用于第二轮压低误报，留空时自动使用初检模型。

修改配置时，API 密钥留空会保留原有的加密密钥，网页和接口都不会回传该密钥。每次新建或修改配置都必须再次确认正文会发送给所选服务商。配置修改只影响之后创建的任务；已创建任务会继续使用创建时保存的配置快照。

## 协议行为

- **OpenAI Responses**：使用原生 Responses 请求体的 `instructions`、`input` 与 `text.format` JSON 模式，并固定发送 `store: false`，避免在支持该字段的服务中保留文章请求状态。模型需要支持 Responses API 与 JSON 模式。
- **OpenAI Compatible**：使用 Chat Completions 的 `messages` 与 `response_format: {"type":"json_object"}`。适合官方兼容端点、API 网关及本地服务；兼容实现的实际能力由部署者负责确认。
- **Anthropic**：使用原生 Messages API 的顶层 `system` 与 `messages`，并设置 `max_tokens: 8192`。为兼容更多 Messages API 模型，首版不强制依赖 Anthropic 的可选结构化输出特性；模型仍会收到严格 JSON 要求，服务器会继续验证结构、必要时进行一次 JSON 修复调用。

上述三种协议共享相同的安全和可靠性边界：超时、429 和 5xx 最多重试三次；响应最多 1 MiB；结构无效时只进行一次“保持语义、不新增判断”的 JSON 修复调用；仍不合法则整个文本块失败。请求、错误和审计日志都不会记录 API 密钥、文章正文或完整提示词。

OpenAI 的 Responses API 支持通过 `text.format` 约束 JSON 输出，并可通过 `store: false` 关闭默认请求状态保留；Anthropic 的 Messages API 使用顶层 `system` 字段而非 `system` 消息角色。实现依据见 [OpenAI Responses API 参考](https://platform.openai.com/docs/api-reference/responses) 与 [Anthropic Messages API 参考](https://platform.claude.com/docs/en/api/messages/create)。

## 密钥、隐私与质量

API 密钥使用 XChaCha20-Poly1305 加密，主密钥只从运行环境读取。用户第一次创建配置必须确认正文会发送给对应服务商；自建部署者需要自行评估服务商隐私条款、数据地域、保留策略、训练用途和跨境传输要求。

候选模型与复核模型可以相同，也可以分别配置。候选阶段先判断上下文中的语言关系，再归类已成立的问题；复核阶段独立判断，不以候选标签为结论。每次报告固定记录服务协议、两个模型名、提示词版本、依据库引用和分析器版本。管理员不能通过公开接口把任意配置标成“参考配置”；该字段只能在完成私有评测后由受控发布流程设置。

只有发布说明列明的参考模型组合可以附带质量指标。用户自带模型在界面和报告中始终显示“未经验证”，不能沿用参考模型的准确率与召回率承诺。

## 提示词版本

报告中的 `zh-cn-proofread-v3` 是当前内置初检与复核提示词的版本标识，用于复现与质量追溯，并不是网页中的可编辑字段。社区 MVP 暂未提供提示词管理接口或页面；不要直接修改数据库中的提示词记录，因为迁移会重新写入当前内置版本。

需要调整提示词时，应在 [`crates/textcomb-core/src/prompts.rs`](../crates/textcomb-core/src/prompts.rs) 中创建新的版本，并在私有评测集上重新验证后再用于质量声明。将来的提示词管理功能必须保留版本、激活记录和历史任务快照，不能覆盖旧报告的追溯信息。
