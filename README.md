# 文梳 TextComb

文梳是面向中文作者的开源辅助校对工具。它分析错字、标点、病句和疑似分段问题，输出可审核的问题报告，不自动改写原文。

## 当前范围

- 中国大陆简体中文。
- 直接粘贴正文、TXT、DOCX、文字型 PDF；单文件不超过 20 MiB、正文不超过 5 万字。
- AI 高召回候选与 AI 二次复核。
- 在线报告、JSON、Markdown 和 PDF 导出。
- 用户自带 OpenAI Responses、OpenAI Compatible 或 Anthropic 模型密钥。

扫描 PDF、OCR、事实核查、论文格式、全文一致性和自动改写不在首版范围内。

## 技术栈

- Rust 1.97.1、Axum、Tokio、SQLx、PostgreSQL 18。
- Vue 3、TypeScript、Vite。
- PostgreSQL 任务队列，不依赖 Redis。
- Poppler 提取 PDF 文字，Typst 生成 PDF 报告。

## 本地启动

最快的开发启动方式：

1. 复制 `.env.compose.example` 为 `.env`，替换数据库密码和 32 字节 Base64 主密钥。
2. 运行 `docker compose up -d --build`；一次性 `migrate` 服务会先执行数据库迁移。
3. 运行以下命令创建唯一的首个超管：

```shell
docker compose --profile tools run --rm cli bootstrap-admin --username admin --password '至少十二个字符的密码'
```

4. 打开 `http://localhost:3000`；如果 `.env` 中改过 `TEXTCOMB_HTTP_PORT`，请使用对应端口。登录后创建自己的 OpenAI Responses、OpenAI Compatible 或 Anthropic 模型配置。
5. 在“新建分析”中选择上传文件或“粘贴正文”，再选择模型配置并开始分析。

完整开发方式、密钥生成命令和故障排查见 [`docs/development.md`](docs/development.md)。模型接口格式、修改配置与质量边界见 [`docs/model-providers.md`](docs/model-providers.md)。架构取舍见 [`docs/architecture.md`](docs/architecture.md)，部署与备份见 [`docs/operations.md`](docs/operations.md)。

> 当前仓库实现的是可部署的开源 MVP 基线。参考模型的 90% / 90% / 70% 质量门槛必须在独立私有评测集上达成后，才能在发布说明中标记为“质量验证配置”。仓库本身不会伪造该认证。

## 质量声明

质量指标只对仓库发布说明中明确列出的参考模型、提示词和依据库版本有效。任意自定义模型配置会显示为“未经验证”。文梳提供辅助建议，最终判断由作者完成。

## 许可证

社区代码采用 GNU AGPL-3.0-or-later。`文梳`、`TextComb` 名称及项目标识受单独的商标政策约束。需要闭源集成或其他授权时，请联系项目维护者。

## 仓库结构

```text
apps/                  API、Worker、管理 CLI
crates/textcomb-core/  解析、队列、模型、报告与安全实现
crates/textcomb-domain 公共 ReportV1 与领域类型
tools/textcomb-eval/   可公开复用的质量评测程序
tools/fake-model/      不调用外部服务的确定性假模型
web/                   Vue 3 工作台
migrations/            PostgreSQL 前滚迁移
load/                  10 篇长文并发负载脚本
```
