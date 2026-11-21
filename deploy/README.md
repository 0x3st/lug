# Lug 部署（Rust v1）

架构：`systemd 守护 lug-server(127.0.0.1) ← Caddy/Nginx 反代 → 自动 HTTPS 对外`。
产物为**单静态二进制**（~1.3MB），服务器无需 Python/Node/Rust 运行时。

## 1. 构建

本机交叉编译（推荐，服务器零工具链）：

```bash
# macOS → Linux x86_64（首次: cargo install cargo-zigbuild && rustup target add x86_64-unknown-linux-gnu）
cargo zigbuild --release --target x86_64-unknown-linux-gnu
# 产物: target/x86_64-unknown-linux-gnu/release/lug-server 与 lugctl
```

或服务器上直接构建：

```bash
git clone <repo> /opt/lug-src && cd /opt/lug-src && cargo build --release
# 产物: target/release/{lug-server, lugctl}
```

## 2. 安装

```bash
sudo mkdir -p /opt/lug/bin /opt/lug/web
sudo cp target/x86_64-unknown-linux-gnu/release/{lug-server,lugctl} /opt/lug/bin/
sudo cp -r web /opt/lug/web            # 前端构建产物（frontend/ 下 npm run build）
# 3. 环境文件（600 权限）
sudo install -m 600 deploy/lug.env.example /etc/lug.env && sudoedit /etc/lug.env
# 4. systemd
sudo cp deploy/lug.service /etc/systemd/system/lug.service
sudo systemctl daemon-reload && sudo systemctl enable --now lug
# 5. 签发首个 API Key（服务端停止时操作；v1-alpha 无控制台登录）
sudo /opt/lug/bin/lugctl create-key my-first-key
```

验证：`curl -s localhost:8000/f/a/o/calculate -H "Authorization: Bearer sk-xxx" -d '{"expression":"1+1"}'`

## 3. 反向代理（Caddy 推荐）

```bash
sudo cp deploy/Caddyfile /etc/caddy/Caddyfile   # 改成你的域名后 reload
# Nginx 用户用 deploy/nginx.conf.example（SSE 需要 proxy_buffering off）
```

## 4. 更新版本

```bash
cd /opt/lug-src && sudo git pull && cargo build --release
sudo systemctl stop lug     # lugctl 与服务端共用 api_keys.json，先停后操作
sudo cp target/release/{lug-server,lugctl} /opt/lug/bin/
sudo systemctl start lug
```

## 5. 前端更新注意事项

- `/etc/lug.env` 必须显式配置 `WEB_DIR=/opt/lug/web`。当前静态资源处理器
  将文件的绝对路径与资源目录比较；相对路径 `web` 会导致 JS/CSS 返回 404，
  即使 HTML 正常返回 200。修改环境变量后需要重启 `lug`。
- 先备份 `web/`，上传新资源并验证**源站与公网**均可访问，再原子替换三个 HTML。
  保留旧哈希资源，避免已打开的页面或缓存 HTML 请求失败。单纯更新静态文件无需重启。
- Cloudflare 可能缓存资源的 404；修复源站后仍需清除相应缓存，或让 Vite
  生成全新资源 URL（包括入口、共享 chunk、CSS 和字体），不要手工改压缩后的引用。

### 2026-09-30：Lab 530 branding

- 站点：`https://lug.by530.com`；前端目录：`/opt/lug/web`。
- 已设置绝对 `WEB_DIR` 并重启服务；未上传后端二进制、未修改密钥配置。
- 为避开部署检查期间缓存的 404，本次构建临时覆盖 Rollup `output`：
  `entryFileNames` / `chunkFileNames` 为 `assets/[name]-[hash]-lab530-r1.js`，
  `assetFileNames` 为 `assets/[name]-[hash]-lab530-r1[extname]`。
  当次使用独立资源名避开错误缓存，后续更新可恢复标准构建。
- 旧页面备份：`/opt/lug/backups/web-lab530-20260930T062428Z`。
  旧环境文件备份：`/opt/lug/backups/lug.env-before-lab530-20260930T062428Z`
  （仅 root 可读；不要上传到仓库）。回退页面时应保留正确的绝对 `WEB_DIR`。
- 验证：三页 HTML 与八个资源逐文件一致；线上 branding 浏览器检查
  （中英文 × 1440/768/390/320px）通过；未认证 API 返回预期 401。

### 2026-09-30 14:43 CST：确认版品牌与产品口径

- 产品定位统一为「结构化计算 AI」，API 仅作为接入方式。
- 导航、页脚和页面标题恢复为 Lug；实验室署名仅保留在首页介绍末尾：
  「面向结构化计算的 AI，支持表达式求值、流式计算过程与用量统计。By Lab 530」。
- 使用 `cd frontend && npm run build` 标准构建；仅更新 `web/`，
  先验证源站与公网全部资源，再原子替换 HTML，保留旧哈希资源。
  未重启服务，后端 PID 与二进制校验和保持不变，未修改环境或密钥配置。
- 上一版备份：`/opt/lug/backups/web-branding-20260930T064327Z`。
- 验证：三个 HTML、八个静态资源与本地构建逐文件一致；线上中英文
  1440/768/561/560/390/320px 六组 branding 检查通过；未认证 API 返回预期 401。

### 2026-10-08 09:26 CST：Lab 530 风格与页脚双品牌

- 首页、文档和控制台统一为 STIX Two Text 标题、米白／深蓝和系统深色模式。
  首页不放大纹章；页脚左侧为「Lug 图标 + Lug ｜ Lab 530 纹章 + Lab 530」。
  Lug 暂沿用原方块 L。保留 AI 产品定位与介绍末尾的 `By Lab 530`。
- 标准构建 `cd frontend && npm run test:ui`；仅更新 `/opt/lug/web`。
  先验证新资源在源站与公网可访问，再原子替换 HTML；旧哈希资源保留。
  未改后端、环境或密钥配置，未重启服务（PID 525、二进制校验和不变）。
- 上一版备份：`/opt/lug/backups/web-lab530-style-20261008T012635Z`。
- 字体使用真实 WOFF2 转换产物；OFL 许可证随包存放于
  `web/assets/stixtwotext-OFL.txt`。纹章与字体许可证彼此独立。
- 验证：三个 HTML 与十个浏览器静态资源逐文件一致；本地和线上各 28 项
  UI 检查通过（中英文、明暗模式、六种宽度、字体/favicon、交互与弹窗）。
  控制台交互检查使用模拟响应，未操作线上密钥；另验证未认证 API 返回 401。

### 2026-10-08 15:01 CST：纯字标与无装饰网格线

- Lug 导航、页脚和演示节点统一使用 STIX 纯文字字标，移除方块 L；
  favicon 改为透明底 `Lug` 文字。Lab 530 纹章保留，链接改为 `https://by530.com/`。
- 保留网格布局与对齐，移除多余边框和章节分隔线；保留表格、表单、
  选中状态、焦点、流程连接及页脚品牌分隔线。
- 用户确认预览后明确授权 publish。标准构建，仅更新 `/opt/lug/web`；
  源站和公网十个浏览器静态资源逐文件验证后，逐个原子替换三个 HTML，
  再确认 HTML 与本地构建一致。保留旧哈希资源，发布暂存文件已清理。
- 上一版备份：`/opt/lug/backups/web-wordmark-20261008T070109Z`。
- 本地与线上各 30 项 UI 检查通过（中英文、明暗模式、响应式、字标、
  链接、网格与功能边界、交互和模拟控制台）。未认证 API 返回预期 401。
- 后端 PID 525、两个二进制及 `/etc/lug.env` 校验和不变；未操作线上密钥，
  未重启服务。OFL 许可证已随包安装并在服务器逐字节校验；现有后端不提供
  `.txt` 静态路由，因此许可证不计入上述十个浏览器资源的 HTTP 检查。

## 6. 安全清单

- [ ] `chmod 600 /etc/lug.env` 与 `/opt/lug/api_keys.json`（Key 哈希与用量）
- [ ] 防火墙只放行 22/80/443；lug-server 只绑 127.0.0.1
- [ ] 反代层速率限制（Caddy `rate_limit` 插件或 fail2ban）
- [ ] 备份 `api_keys.json`（撤销/用量状态都在这一个文件里）
- [ ] GitHub OAuth 迁移前，`/platform` 登录不可用属预期（见 README 实施状态）
