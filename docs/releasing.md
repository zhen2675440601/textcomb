# 版本准备与发布

当前仍是开源 MVP，尚无质量认证的参考配置。准备候选版本不代表发布，也不授予任意模型“经过验证”的状态。

## 候选版本

1. 按 Issue/PR 完成范围与验收记录。待发布修复必须先合并到 `main`；确认没有未解决的权限、原文丢失、报告串号或不完整报告伪装成功等问题。
2. 保持 Rust workspace 与前端版本一致，更新 Changelog、必要的迁移/公共契约/生成类型和升级说明。未验证阶段采用 `v0.1.0-alpha.1` 等显式预发布名称。
3. 在同一完整提交上通过 CI 的 rust/web/migrations/container/release-tools 和 Security 的 Rust/frontend dependency audit。扫描失败、缺失、跳过、另一个提交或旧运行的通过结果不能代替当前证据。未修复依赖公告须处理并重新验证；不要修改门槛或忽略公告来发版。
4. 在 GitHub Actions 手动运行 **Prepare release candidate**，填写已在 main 上的完整 SHA 与预发布版本。工作流权限仅为 contents/checks 读取；验证精确提交的最新检查，生成源码包、`SHA256SUMS` 和 `candidate.json`，保存为短期 CI 制品。不自动创建标签、Release 或推送镜像。

本机等价命令（先取得 GitHub 的真实检查结果，不手造通过记录）：

```shell
gh api 'repos/zhen2675440601/textcomb/commits/FULL_SHA/check-runs?per_page=100' > ../candidate-checks.json
python -B -m unittest discover -s tools/release -p 'test_*.py'
python -B tools/release/prepare.py --version v0.1.0-alpha.1 --commit FULL_SHA --checks ../candidate-checks.json --output private-evaluation/release-candidate
```

脚本要求工作树干净、当前 HEAD 与目标 SHA 相同、目标在 `origin/main` 历史内且产品版本一致。它核对 GitHub Actions 七个必需检查，拒绝已跳过或失败的结果。检查 JSON 是可信 API 输入，离线手写的 JSON 不能作为发布证据。

源码包采用 Git 的 NUL 分隔路径，按 POSIX 路径检查整个提交，禁止任何层级的 `.env` 与 `.env.*` 实际配置；只允许明确的 `.env.example` 和 `.env.compose.example` 模板名。递归保留目录规则禁止 `private-evaluation`、`data`、`uploads`、`tmp`、`logs`、`backups`、`.sqlx`、`target`、`node_modules`、`dist`、`build`、`__pycache__` 和 `coverage`，并拒绝 `.log`、`.tmp`、`.dump`、`.backup`、`.pyc`、`.pyo` 文件。可再分发的 TXT、PDF、DOCX 测试样例正常保留；环境模板必须使用占位值，模板名和路径检查不能证明文件内容不含密钥或私有正文，仍须维护者审核。当前源码包不支持符号链接或子模块，遇到这些条目会失败，而不会静默遗漏或跟随外部文件。路径必须为 UTF-8，不能含控制字符、反斜杠或冒号。

脚本也检查实际 tar 的文件集和 Git blob 内容；`export-ignore` 隐藏文件或 `export-subst` 改写内容会被拒绝。归档在临时裸仓库中只读共享源对象库，仅使用目标提交的属性；主机的系统/全局 Git 配置、全局属性、未跟踪的 `.git/info/attributes` 及 Git 环境覆盖不会影响归档，也不会改写原仓库配置。仅目标提交明确的 `eol=crlf` 且非 `-text` 属性允许 CRLF 表示转换；其他内容必须与 Git blob 相同。固定 tar 权限掩码和换行设置，压缩时间固定为 0，同一提交/版本可重复生成相同源码校验和。每次须使用尚不存在的输出目录：源码包、`SHA256SUMS` 和 `candidate.json` 在同级临时目录全部写入成功后才一起移入目标目录，失败不会留下半个成功候选包，也不会覆盖旧版证据。

Docker 构建保留依赖缓存，但先清理本工作区产物，避免不同分支 COPY 文件的时间戳导致复用旧项目二进制。

## 维护者最终审阅

发布说明记录实际版本/SHA、已合并的改动、兼容性、验证命令、依赖扫描、升级与恢复结果、数据安全影响和限制。核对源码包与校验和，确认完整 AGPL 许可及部署界面的源码链接可达。外部贡献必须完成现行指南要求的许可协议；目前该流程尚未启用，不能接受外部代码进入发行分支，也不能把勾选模板视为签署法律协议。协议内容和签署方式由维护者确定。

Alpha 可以明确标为未通过参考质量验收的试用版本，但仍须满足工程与安全检查。Beta/稳定质量声明还须至少 50 篇代表性完整文章的双人独立标注、第三人裁决，固定配置连续两轮达到 90%/90%/70%，并完成建议质量与保护区人工审核。应记录自然文章与植入测试的区别、失败样本和分段建议指标，不用单篇或假模型宣称总体质量。

维护者确认上述候选包和说明后，再从该精确提交创建预发布标签与 Draft Release；审阅 Draft 的版本、来源、制品和限制后发布。未通过检查时保留候选失败原因，不创建对外成功版本。

## 升级与恢复演练

在隔离环境使用与生产相同的 PostgreSQL 主版本及应用镜像。暂停写入和 Worker，验证备份/恢复后报告数量、JSON/Markdown/PDF、原文位置、权限、会话失效、清理和前滚迁移。数据库备份与对应主密钥分别安全保管；缺少密钥不能恢复模型配置。备份包含正文和密钥密文，必须按报告保留期管理，不上传 CI 或 Release。原文件 `uploads/`、临时目录与已删除正文不能混入长期备份。

运维步骤见 [部署与运维](operations.md)。先备份，再使用固定标签或摘要重建镜像；迁移单独前滚，检查 API readyz，再启动 Worker 和入口服务。应用回退不执行迁移回滚；不兼容迁移不得在同一版删除旧字段。
