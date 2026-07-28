# Changelog

所有值得关注的变化记录在此。项目遵循语义化版本。

## [Unreleased]

### Added

- TextComb 模块化单体、PostgreSQL 租约队列与双轮 AI 分析流水线。
- TXT、DOCX、文字型 PDF 提取和可逆来源定位。
- 登录、管理员用户、BYOK 模型配置、异步进度、取消与失败块重试。
- 在线问题报告及 JSON、Markdown、PDF 导出。
- Vue 3 中文工作台、Docker Compose、CI、公开评测工具和负载测试脚本。
- 新建分析支持直接粘贴正文，按 UTF-8 TXT 进入既有定位、异步分析与清理链路。
- 模型服务支持 OpenAI Responses、OpenAI Compatible 和 Anthropic 三种接口格式。
- 模型配置支持修改；编辑时留空 API 密钥会保留原有加密密钥。

### Changed

- 报告和任务快照记录模型协议与提示词版本；当前内置提示词版本为 `zh-cn-proofread-v1`。
- 快速启动与开发文档补充自定义 HTTP 端口、显式 CLI 密码参数和 OpenAPI 类型生成说明。
