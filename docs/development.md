# 开发环境

## 推荐方式：Docker Compose

需要 Git、Docker Desktop（或 Docker Engine＋Compose）以及至少 4 GiB 可用内存。

容器发布构建默认使用两个 Rust 编译任务，降低本机同时链接 API、Worker 和 CLI 时的内存峰值。内存充裕的 CI 可通过 `docker compose build --build-arg CARGO_BUILD_JOBS=4` 调整并行度。

```shell
cp .env.compose.example .env
docker compose up -d --build
```

Windows PowerShell 可用以下命令生成主密钥；输出值写入 `.env` 的 `TEXTCOMB_MASTER_KEY_BASE64`：

```powershell
$key = New-Object byte[] 32
$rng = [Security.Cryptography.RandomNumberGenerator]::Create()
$rng.GetBytes($key)
$rng.Dispose()
[Convert]::ToBase64String($key)
```

数据库迁移由一次性 `migrate` 容器先执行。首次创建超管：

```shell
docker compose --profile tools run --rm cli bootstrap-admin --username admin --password '至少十二个字符的密码'
```

常用命令：

```shell
docker compose logs -f api worker
docker compose --profile tools run --rm cli doctor
docker compose --profile tools run --rm cli create-user --username author01 --password '至少十二个字符的密码'
docker compose down
```

`docker compose down` 不删除数据库或报告卷。只有明确执行 `docker compose down -v` 才会删除命名卷；不要在需要保留数据的环境执行后者。

## 本机开发

固定工具链：Rust 1.97.1、Node 24 LTS、PostgreSQL 18、Poppler、Typst 0.14.2。Windows 本机编译 Rust 还需要 Visual Studio Build Tools 的“使用 C++ 的桌面开发”；不想安装时可直接使用容器构建。

Poppler 需提供 `pdfinfo`、`pdftotext`、`pdfimages` 和 `pdftocairo`。后两个工具先检查图像元数据，再核对空白或短文字页，不执行 OCR；扫描页或无法确认内容完整的图像/轮廓页仍不支持。运行 `textcomb-cli doctor` 可检查四个工具及 Typst。

PDF 报告还需要覆盖正文字符的系统字体。容器安装 Noto CJK、Hanazono 和 Noto Color Emoji，分别覆盖常用中文、扩展汉字和 Emoji；本机运行 Typst 时也要安装这些字体或覆盖相同字符的替代字体。只有 Typst 返回成功不能证明字符完整，CI 使用真实 PDF 文字提取检查原文、建议及漏检字段中的补充平面字符和组合 Emoji。

后端：

```shell
cargo run -p textcomb-cli -- migrate
cargo run -p textcomb-api
cargo run -p textcomb-worker
```

前端：

```shell
cd web
npm ci
npm run dev
```

API 运行后可从 Rust 服务公开的规范重新生成前端契约类型：

```shell
cd web
npm run generate:api
```

生成命令默认读取 3000 端口；若使用其他端口，可直接运行 `npx openapi-typescript http://localhost:<端口>/api/openapi.json -o src/api/schema.d.ts`。生成结果是 `web/src/api/schema.d.ts`。CI 会在可部署容器栈上重新生成并检查差异，接口有变化时必须一并提交该文件。

开发服务器默认将 `/api` 代理到 `http://localhost:3000`。容器化开发建议直接使用 Caddy 暴露的 `TEXTCOMB_HTTP_PORT`；未修改 `.env` 时默认是 3000。若该端口已被占用，可把 `.env` 中的 `TEXTCOMB_HTTP_PORT` 改为例如 `18080`，并同步把 `TEXTCOMB_PUBLIC_ORIGIN` 改为 `http://localhost:18080` 后重新执行 `docker compose up -d --build`。

## 确定性假模型

`tools/fake-model/server.mjs` 实现最小 OpenAI Chat Completions 兼容接口，可识别仓库内几条病句样例，不访问互联网：

```shell
node tools/fake-model/server.mjs
```

在文梳中创建模型配置：地址 `http://host.docker.internal:4010/v1`（Worker 在 Docker 中时），密钥填任意非空测试值，模型名填 `fake-textcomb`。假模型只用于集成与负载测试，不能用于质量评测。

## 浏览器工作流回归

前端使用 Node 24。安装依赖后运行：

```shell
cd web
npx playwright install chromium
npm run test:e2e
```

默认启动本地测试前端，以模拟 API 检查历史分页/搜索、断线恢复、报告审核和示例引导，不访问第三方模型。`npm test` 还覆盖重连定时器清理、请求取消、过期登录停止重试及键盘输入保护。

容器 CI 另在独立数据库和假模型上运行真实链路：登录、模型连接测试、提交正文、生成报告、原文定位、审核反馈和 JSON/Markdown/PDF 下载。CI 创建专用 `e2e-admin` 测试账号。手工重跑请使用独立测试 Compose 项目和端口，设置 `TEXTCOMB_E2E_BASE_URL`、`TEXTCOMB_E2E_LIVE=1`，以及该测试环境的 `TEXTCOMB_E2E_USERNAME` / `TEXTCOMB_E2E_PASSWORD`。假模型地址默认为 `http://fake-model:4010/v1`，可用 `TEXTCOMB_E2E_FAKE_MODEL_URL` 覆盖；运行 `npm run test:e2e -- --grep 'real fake-model'`。不要将这些测试账号或配置写入真实用户环境。

本机前端代理可通过 `TEXTCOMB_API_ORIGIN` 指向测试后端，默认为 `http://localhost:3000`。后端的 `TEXTCOMB_PUBLIC_ORIGIN` 需与浏览器实际访问的源一致。上述工程测试不计算语言质量；参考配置仍须执行 `evaluation-preparation.md` 中的独立人工标注与两轮验收。

## 历史与审核交互契约

`GET /api/v1/analyses/history` 返回 `items`、搜索/状态筛选后的 `total`、有效 `page` / `page_size` 和当前用户的全量 `totals`。默认每页 20 条，最多 100 条；`q` 最多 100 字，按文章名不区分大小写匹配，`%`、`_` 和反斜线均为普通字符。`status` 支持 `all`、`active`、`completed`、`failed`、`cancelled`、`expired`，其中 `active` 包含正在取消。超出末页时返回末页；无记录时页码为 1。接口在同一数据库快照中读取计数和记录，按创建时间和 ID 降序排序，并同时返回文档名称和格式。原 `/analyses` 列表保持兼容。

报告快捷键：J/K 上下条、N 下个未审核、1/2/3 正确/错误/有争议；输入框、按钮、链接、组合输入与浏览器修饰键不触发。反馈成功后才计入审核进度，可关闭自动跳转。阅读位置按账号和报告区分，浏览器仅保存问题 ID 与面板开关；存储受限时其余审核功能仍可使用。

## 提交前检查

```shell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cd web
npm ci --ignore-scripts
npm audit --omit=dev --audit-level=high
npm test
npm run build
```

真实 AI 评测不在普通 PR 中运行。评测流程见 [`evaluation.md`](evaluation.md)。

## PostgreSQL 依赖与嵌入迁移

SQLx 主库、core 和 PostgreSQL 驱动固定为同一版本。UUID、时间、JSON 和迁移功能直接在 PostgreSQL 驱动启用，避免通用功能开关将未使用的 MySQL/RSA 或 SQLite 带进锁文件。新增 SQLx 功能或升级版本后，使用 Cargo 更新锁文件，并运行 `python3 -B -m unittest discover -s tools/security -v` 和完整 Rust 安全审计；不要手工移除依赖记录。

`textcomb-core/build.rs` 使用 SQLx 的官方目录解析器，将 `migrations/` 下的前滚 SQL 嵌入二进制。新增或修改文件会触发重新编译；运行镜像不依赖源码目录。运行时仍由 SQLx Migrator 执行事务、数据库锁、校验和与历史验证，不能修改已应用的迁移来消除校验失败。CI 在真实 PostgreSQL 上验证重复迁移、与目录解析器的兼容以及校验和和未知版本的拒绝行为。

正文清理回归测试会扫描该连接能看到的所有文档，因此每项测试使用独立生成的 PostgreSQL schema，并在其中执行真实迁移；单独的 UUID 不能隔离并行清理。测试仍并行执行，重试与清理的竞争也仍在同一项测试内验证。成功后删除该测试 schema，不删除数据库或其他 schema。
