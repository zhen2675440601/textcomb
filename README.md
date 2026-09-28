# 文梳 TextComb

文梳（TextComb）是一款面向中文写作者的开源辅助校对工具。它帮助作者检查错字、标点、病句和疑似分段问题，并将问题与原文位置关联，供作者自行判断；它不是自动改写器，也不承诺替代人工审校。

项目当前以自媒体作者为主要用户，同时支持论文、新闻稿和财报等长文场景。欢迎先从身边的真实文章开始试用，并通过问题报告和漏检反馈帮助改进识别质量。

## 能做什么

- 粘贴正文，或导入 TXT、DOCX、文字型 PDF；单文件最大 20 MiB，正文最多 20 万字。
- 检查错字、标点、病句，以及低置信度的疑似分段建议。
- 使用 AI 先找问题、再复核；报告提供问题类别、原文片段与位置、原因、修改建议、置信度和可用依据。
- 查看带问题定位的分析原文，逐条标记问题是否正确，也可标记模型漏掉的问题。
- 在线浏览报告，或导出 JSON、Markdown、PDF；不会导出修改后的全文。
- 配置自带模型：OpenAI Responses、OpenAI Compatible 或 Anthropic Messages。正文会发送到用户所配置的模型服务商。
- 为通用文章、学术论文和财报/商业报告选择分析场景；长文按块处理，并保留必要的上下文和原文位置。

目前不支持扫描版 PDF、OCR、事实核查、论文格式检查、跨章节一致性检查或自动改写。加密、损坏或无法提取文字的 PDF 会被拒绝。

## 质量与使用边界

模型输出是辅助建议，作者需要自行审核。不同模型、提示词及服务商对结果影响很大，因此自定义模型配置显示为“未经验证”；目前没有对所有模型作准确率承诺。项目的正式发布门槛是：参考配置正式问题准确率至少 90%、正式加疑似问题召回率至少 90%、疑似问题准确率至少 70%，必须通过私有评测集验证后才能宣称达标。

2026-09-28 的一次公开文章探索性试跑和长文/队列验证记录在 [`docs/evaluation-run-2026-09-28.md`](docs/evaluation-run-2026-09-28.md)。公开文章样本和少量植入错误只能用于排查流程问题，不构成质量认证；目前的测试结论与后续计划见 [`docs/STATUS.md`](docs/STATUS.md) 和 [`docs/evaluation.md`](docs/evaluation.md)。

## 快速启动

需要 Docker Compose。首次启动前，在项目根目录复制 `.env.compose.example` 为 `.env`，修改数据库密码并生成 32 字节 Base64 主密钥。具体生成方式见 [`docs/development.md`](docs/development.md)。

```shell
docker compose up -d --build
docker compose --profile tools run --rm cli bootstrap-admin --username admin --password '请替换为至少十二个字符的密码'
```

打开 `http://localhost:3000` 登录。若在 `.env` 中修改了 `TEXTCOMB_HTTP_PORT`，请使用对应端口。首个超管可创建后续用户；登录后到模型设置中添加并测试模型配置，再新建分析。

不要把 `.env`、主密钥或模型密钥提交到仓库。有关开发环境、前后端启动和常见问题，请看 [`docs/development.md`](docs/development.md)。

## 技术概览

- 后端：Rust 1.97.1、Axum、Tokio、SQLx。
- 前端：Vue 3、TypeScript、Vite。
- 数据库与任务队列：PostgreSQL 18；不依赖 Redis。
- 文档处理：Poppler 提取文字型 PDF；DOCX/TXT 解析保留来源位置。
- PDF 报告：Typst 按请求生成。
- 部署：Docker Compose；API 与 Worker 分离运行，任务由 PostgreSQL 队列调度。

```text
apps/                  API、异步 Worker、管理 CLI
crates/textcomb-core/  分析流程、文档解析、模型适配、存储与报告
crates/textcomb-domain 公共领域类型与 ReportV1
web/                   Vue 3 工作台
migrations/            PostgreSQL 数据库迁移
tools/textcomb-eval/   可复用的评测程序
tools/fake-model/      确定性测试模型，不调用外部 AI 服务
load/                  长文并发负载测试脚本
docs/                  产品方向、架构、开发、运维与评测说明
```

长文处理设计见 [`docs/architecture.md`](docs/architecture.md)；模型配置见 [`docs/model-providers.md`](docs/model-providers.md)；部署、数据保留和备份见 [`docs/operations.md`](docs/operations.md)。负载脚本默认生成 5 万字文章，可用 `TEXTCOMB_LOAD_CHARS=200000` 检查上限，并通过 `TEXTCOMB_ANALYSIS_PROFILE=academic` 或 `financial` 选择场景。性能和真实模型成本仍需在目标部署环境中测量，不应仅凭假模型压测推断。

## 开发与贡献

Rust、Node、数据库和本地运行的详细版本要求见 [`docs/development.md`](docs/development.md)。CI 和提交前检查说明也在那里。产品目标、当前状态与路线图分别见 [`docs/PROJECT.md`](docs/PROJECT.md)、[`docs/STATUS.md`](docs/STATUS.md) 和 [`docs/ROADMAP.md`](docs/ROADMAP.md)。欢迎提交问题、复现样例和改进建议；请勿在公开 Issue、PR 或日志中附上未获授权的文章正文、密钥或个人信息。

## 许可证

社区代码采用 GNU AGPL-3.0-or-later，详见 [`LICENSE`](LICENSE)。`文梳`、`TextComb` 名称及项目标识受单独的商标政策约束。需要闭源集成或其他授权时，请联系项目维护者。
