# 漫画高清化 Agent 设计与落地方案

> 目标：把本地“按顺序图片 → 统一重命名 → 上传到远端 → 2×放大与降噪 → 产物按“作品名/卷/图片”结构回传”的全流程，沉淀为 reiChan 工具集中的一类 Agent（Tauri 前端 + Windows 端 Python 服务），不再依赖 ComfyUI。

## 当前进度（2025-09-27）

- ✅ **M1 完成**：
  - Rust 侧提供 `rename_manga_sequence` / `upload_copyparty` command（自然排序重命名、生成 `manifest.json`、zip 打包上传），并配套 `httpmock` + `tempfile` 单元测试，`cargo test` 全量通过。
  - 前端新增 “Manga Upscale” Agent UI，串联重命名预览/执行与 Copyparty 上传，支持系统目录选择（通过对话框插件命令）与状态提示。
  - `services/manga_upscale_service` 目录落地 FastAPI 雏形，提供 `POST /jobs`、`GET /jobs/{job_id}`、`/health`、`/models`，模拟推理进度与 artifact 命名。
- ✅ **M2（作业看板 / 并发队列 / 产物回传）**：
  - Tauri 前端新增 “步骤 3” 作业面板，支持从 Copyparty 上传参数一键带入、提交 FastAPI 推理作业，并以 WebSocket 优先、轮询兜底的方式展示进度与产物链接。
  - Rust 后端新增 `create_manga_job`、`fetch_manga_job_status`、`watch_manga_job` 命令，基于 `tokio-tungstenite` 订阅 `/ws/jobs/{job_id}`，事件落地为 `manga-job-event` 统一广播。
  - FastAPI 服务引入 `REICHAN_MAX_CONCURRENCY` 并发控制、内存队列、实时订阅列表以及占位 zip 产物回传；`GET /jobs/{job_id}/artifact` 直接输出 zip 文件。
  - Step2 上传过程提供实时进度广播（本地打包、流式上传、完成），前端使用 `manga-upload-progress` 事件显示百分比与状态。
  - 前端 Stepper 增加“目录分析 + 卷映射”预处理，自动识别多卷父目录并允许用户确认卷号与展示名称，后端新增 `analyze_manga_directory` 命令输送卷元数据。
- ✅ **M3（参数面板 / 恢复 / 产物校验 初版）**：
  - Step3 推理参数面板提供模型、放大倍率、降噪、格式与高级选项输入，支持默认值持久化与最多 8 组收藏预设；作业列表补齐状态筛选、关键字搜索与批量操作按钮。
  - Rust 后端新增 `resume_manga_job`、`cancel_manga_job`、`download_manga_artifact`（纯下载）与 `validate_manga_artifact`（校验）命令，支持 SHA-256 校验、manifest 对账与 `artifact-report.json` 导出。
  - FastAPI 模拟服务加入 Resume/Cancel/Report 端点与 ETag 缓存逻辑，Resume 时自动重置进度与产物缓存。
- ⚠️ **环境提醒**：当前 dev 机默认 Node v14.18.3，运行 `npm run build` 需切换至 Node ≥ 18.17（可使用 `/Users/natsurei/.nvm/versions/node/v22.14.0/bin`）。
- 🚧 **下一步（M3 后续迭代）**：聚焦对账报告明细展示、冒烟脚本、失败分类/监控整合等收尾工作，详见里程碑拆分。

## 总览
- 本地（macOS）侧：
  - 选取漫画图片文件夹；按 4 位序号统一重命名为 `0001.jpg`、`0002.jpg` …（可回滚）。
  - 通过内置上传器发往远端 Copyparty（或 WebDAV/HTTP 接口），携带元数据（作品名、卷名、期望模型、参数）。
  - 触发远端 Python 服务创建推理 Job，并实时查看进度；完成后按作品结构打包下载回本地。
- 远端（Windows）侧：
  - FastAPI 提供 Job 队列 + 推理执行（优先 PyTorch/ONNXRuntime 跑 Real-ESRGAN 动漫模型，支持 2× 放大与降噪）。
  - 输出目录结构：`{title}/{volume}/0001.jpg` …；可选生成同名 `.zip` 用于回传。

```
mac 本地(Tauri) ──重命名/清单/打包──▶ Copyparty(Windows磁盘)
         │                                   │
         └────创建Job/轮询/WS──▶ FastAPI 服务 ├──▶ JobWorker(ESRGAN 推理)
                                         │   └──▶ 输出与打包(zip)
                                         └◀──下载产物/验证── Tauri 本地
```

## 目录与命名约定
- 本地工作目录（举例）：
  - `source/` 原始图片；`work/` 工具重命名后的快照；`manifest.json` 重命名映射与校验摘要。
- 远端仓库（Windows）：
  - `incoming/` 来自 Copyparty 的上传根；
  - `staging/` 正在处理的 Job 输入快照；
  - `outputs/{title}/{volume}/` 处理结果；
  - `artifacts/` 打包产物（zip）；
  - `models/` 预训练权重；`logs/` 运行日志；`cache/` 临时中间件。
- 图片命名：固定 4 位零填充序号（`0001.jpg`）。如需更多页数，仍统一 4 位（超 9999 的情况另行拆卷或扩位，UI 需预警）。
- 多卷输入：允许父目录下包含多个卷子目录，工具会按自然排序与文件名中的数字尝试推断卷号，并在卷映射步骤中给出可编辑的预设。

## Tauri 前端 Agent（UI/交互）
- 交互步骤（Stepper）：
  1) 选择并分析本地源目录，统计根目录图片/子目录，并区分单卷或多卷场景。
  2) 若检测到多卷结构，自动推测卷号并展示卷映射表，允许用户确认与修改（可重新排序、修正卷名）。
  3) 一键重命名目标卷，输出 `manifest.json`（可干跑预览与回滚）。
  4) 配置上传目的地（Copyparty 配置档：URL、路径、鉴权）、作品名、卷名/卷选择、推理参数；执行上传（zip/FOLDER 预留）。
  5) 触发远端 Job，展示队列和进度（轮询或 WebSocket 实时日志）。（M2）
  6) 作业完成后下载 zip，自动解压到目标目录，做完整性校验（数量/尺寸/哈希）。（M3）
- 后端命令（Tauri 后端 Rust 命令建议）：
  - `analyze_manga_directory(path)`：扫描目录，统计根级图片、子目录图片数，推测卷号并返回忽略列表，为多卷流程提供元数据。
  - `rename_sequential(dir, pad=4, ext="jpg", dry_run=false)`：稳定排序、合法化扩展名、生成回滚映射与 `manifest.json`。
  - `zip_dir(src, out_zip, compression="deflate")`：可选打包以提速上传。
  - `upload_copyparty(profile, src, dest_path, mode={folder|zip}, concurrency=N)`：支持断点续传与重试（ETag/Content-Range 或分片策略，按 Copyparty 能力实现）。
  - `create_job(service_url, token, payload)`：把上传路径与参数交给远端服务创建 Job。（M2）
  - `job_events(job_id)`：优先使用 WebSocket；否则退化为轮询 `get_job`。（M2）
  - `download_artifact(job_id, save_to)`：获取并解压产物；与 `manifest.json` 做对账。（M3）
- UI 加值：
  - 失败重试按钮、Resume 上传、网速与 ETA 预估；
  - 处理参数预设（例如“Anime 2× + Denoise 中等”）。

## Windows 推理服务（Python + FastAPI）
- 运行时选型：
  - 框架：FastAPI（API/WS）+ Uvicorn（ASGI 服务）。
  - 推理后端（按机器环境择优）：
    - PyTorch + Real-ESRGAN（官方 PyTorch 脚本/权重）；
    - ONNX Runtime（如有现成 onnx 模型，配置 `CUDAExecutionProvider` / `DmlExecutionProvider` / CPU 回退）；
    - 兜底：`realesrgan-ncnn-vulkan` 外部可执行（若 Python 依赖难以满足）。
- 环境与配置（建议）
  - Python 3.10+；GPU 优先（NVIDIA CUDA / Windows11 + DirectML 均可）。
  - 环境变量（.env）：
    - `REICHAN_BIND=0.0.0.0:8001`
    - `REICHAN_STORAGE_ROOT=D:\\rei-warehouse`
    - `REICHAN_TOKEN=***`（简单 Bearer 校验）
    - `REICHAN_BACKEND=torch|onnx|ncnn`
    - `REICHAN_PROVIDER=cuda|dml|cpu`（onnxruntime 执行提供者）
    - `REICHAN_MAX_CONCURRENCY=1`（按显存与磁盘 I/O 限流）
- API 设计（草案）
  - `GET /health`：返回进程信息、后端/设备可用性；
  - `GET /devices`：CPU/GPU/Provider 能力枚举。（M2）
  - `GET /models`：列出可用模型（如 `RealESRGAN_x4plus_anime_6B` 等）；
  - `POST /jobs`：创建作业
    - 请求：
      ```json
      {
        "title": "作品名", "volume": "卷名",
        "input": {"type": "folder", "path": "incoming/xxx"},
        "params": {
          "scale": 2,
          "model": "RealESRGAN_x4plus_anime_6B",
          "tile": 512, "tile_pad": 32,
          "denoise": "medium",
          "format": "jpg", "quality": 95, "subsampling": "4:4:4"
        }
      }
      ```
    - 响应：`{ "job_id": "..." }`
  - `GET /jobs/{job_id}`：状态与进度（`PENDING|RUNNING|SUCCESS|FAILED|CANCELED`，已处理/总数、当前页、ETA、错误列表）。
  - `POST /jobs/{job_id}/cancel`：尝试终止。（M3）
  - `GET /jobs/{job_id}/artifact`：下载 zip；或返回产物目录索引。（M2）
  - `WS /ws/jobs/{job_id}`：推实时日志与进度打点（避免频繁轮询）。（M2）
- 执行流水线
  1) 入库：从 `incoming/...` 复制到 `staging/{job_id}`，同时快照 `manifest.json`。
  2) 处理：按 `0001.jpg`…顺序逐张：
     - 用 Pillow 打开并规范到 `RGB`（保留 EXIF/ICC 尽可能传递）；
     - 调用后端推理：
       - PyTorch/Real-ESRGAN：可直接走官方推理脚本的 Python 入口，或将其封装为内部函数；
       - ONNX Runtime：加载对应 onnx，设置 providers：`['CUDAExecutionProvider','CPUExecutionProvider']` 或 `DmlExecutionProvider`；
       - 对大图启用分块：`tile=512, tile_pad=32`，降低显存压力；
     - 输出至 `outputs/{title}/{volume}/0001.jpg`，JPEG 质量 `95`、`4:4:4` 子采样（尽量减少文本边缘色带）。
  3) 打包：生成 `{title}-{volume}-{job_id}.zip` 写入 `artifacts/`，并返回下载链接。（M2）
  4) 清理：保留 `staging` 与日志一定天数（例如 7 天），定期清理。
- 错误与恢复
  - 单张失败：记录错误并继续（可在 UI 中高亮）；
  - Job 重试：使用相同输入路径 + 去重键（基于文件列表与大小/哈希）避免重复处理；
  - 资源保护：按 `REICHAN_MAX_CONCURRENCY` 限制并串行占用 GPU；
  - 终止：收到取消请求时在 tile 边界检查中断。
- 安全
  - 简单 Bearer Token；仅内网开放；CORS 允许 Tauri 源；
  - 禁止目录穿越；所有路径均在 `REICHAN_STORAGE_ROOT` 内解析。

## 模型与质量策略
- 默认模型：`RealESRGAN_x4plus_anime_6B`，`scale=2`（适合漫画线稿与网点，兼顾降噪）。
- 质量参数：
  - JPEG：`quality=95`、`subsampling=4:4:4`，可选 `progressive=true`；
  - 透明通道：如输入包含透明（少见于漫画），统一输出 PNG。
- 性能参数：
  - `tile/tile_pad` 视显存动态调优；
  - ONNX Runtime provider 优先顺序：`CUDA`/`DML` → `CPU` 回退。

## 与 Copyparty 的集成建议
- 以“目标根路径 + 作品名 + 卷名”组织上传目录，或上传一个 zip（UI 可选）。
- 上传策略：
  - 小图多文件 → 打包 zip 更快；
  - 大图少文件 → 直传也可；
- 断点续传：优先采用分片/多段上传能力（依 Copyparty 配置）；如不可用则通过“已存在文件跳过 + 校验和”实现幂等。

## 测试策略（Test First）
- 前端：
  - 重命名器的单测（自然排序、扩展名统一、回滚正确性、manifest 校验）。
  - 上传器的集成测试（模拟 5% 丢包/失败重试）。
- 后端：
  - Job 生命周期与并发控制；
  - 单张图的推理快照对比（金图像素校验/结构相似度 SSIM 采样）。
  - 大图 OOM 保护：tile 模式覆盖测试。

## 里程碑拆分
- ✅ M1（本周）：本地重命名 + 上传器 + 后端雏形（当前使用模拟推理，输出固定 2× artifact 路径）。
- M2：前端 Job 看板 + WS 实时进度；后端作业队列与并发控制、Zip 回传。
- M3：参数面板（模型/降噪/格式）；错误恢复与 Resume；产物校验与对账报告。详见下方拆解。

## M3 功能拆解

- **Tauri 前端（Step 3/4/5/6）**
  - ✅ 参数面板：提供模型下拉（含默认/收藏）、放大倍率、降噪等级、输出格式与 JPEG 质量等选项，支持默认值持久化与一键重置。
  - ✅ 高级选项：允许配置 tile、tile pad、batch size、推理设备偏好，并在 UI 中展示提示，调节显存与性能。
  - ✅ 错误恢复：支持在 WebSocket 中断或作业失败后触发 Resume，保留重试次数与最近失败原因。
  - ✅ 产物校验：下载 zip 后自动解压到目标目录，依据 manifest 生成对账统计与 `artifact-report.json`。
  - ✅ 批量管理：列表可按状态筛选、关键字搜索，并执行批量 Resume / Cancel / Download。
- **Rust 后端命令层**
  - ✅ 扩展 `create_manga_job` 输入契约，携带参数面板结果并记录输入源信息。
  - ✅ 新增 `resume_manga_job` / `cancel_manga_job`，调用 FastAPI 恢复/终止端点并保留上下文。
- ✅ 新增 `download_manga_artifact`（下载产物）与 `validate_manga_artifact`（校验任务），完成 SHA-256 校验、manifest 对账与报告生成。
  - ✅ 单元测试：覆盖参数默认值、artifact 匹配/不符以及 resume/cancel 请求路径。
- **FastAPI 服务（Windows 端）**
  - ✅ Job 模型扩展：保存参数、上传元数据、重试计数与 artifact 哈希，Resume 会重置进度与产物缓存。
  - ✅ 新增 `POST /jobs/{job_id}/resume`、`POST /jobs/{job_id}/cancel`、`GET /jobs/{job_id}/report`，并支持 ETag 缓存策略。
  - ⏳ 推理执行器：后续补齐阶段化 Resume、失败分类与真实并发排程。
  - ⏳ 监控与日志：待整合结构化日志与 `/health` 扩展指标。
- **验证与运维**
  - ⏳ 端到端冒烟脚本：自动验证上传→推理→校验全链路。
  - ✅ 文档与对账报告样例：产出 `artifact-report.json`，补充参数与恢复指引。
  - ⏳ 指标采集：规划 Prometheus/Elastic 对接。

## 风险与备选
- 若 PyTorch 依赖/显卡环境复杂：短期以 `realesrgan-ncnn-vulkan` 可执行兜底；Python 仅做调度。
- 若 Copyparty 上传能力受限：启用 WebDAV 客户端或改为纯 zip 传输。

## 附：关键库/能力要点（摘记）
- FastAPI：CORS 中间件（允许 Tauri 源）；BackgroundTasks 或后台 Worker 执行；支持 WebSocket 推流；可在启动/停止时加载与释放模型。
- Uvicorn：开发用 `--reload`，生产可 `--workers N`；需要证书时可交由反向代理或用 Gunicorn+UvicornWorker。
- ONNX Runtime：`InferenceSession(..., providers=["CUDAExecutionProvider","CPUExecutionProvider"])`；可选 `DmlExecutionProvider`；大吞吐可用 IOBinding 降低拷贝。
- Real-ESRGAN（Anime）：提供 PyTorch 推理脚本与 tile 推理参数；也有 ncnn 可执行用于 Windows 免依赖部署。
- Pillow：保存 JPEG 时优先 `quality=95` + `subsampling="4:4:4"`（文本边缘更干净），尽可能保持 ICC/EXIF。

---

### 下一步
- 如你确认此方案，我可以：
  1) 在前端新增 “Manga Upscale” Agent 的 UI 框架（不改动 CI/Secrets）；
  2) 在 `src-tauri` 添加 `rename_sequential` 与 `upload_copyparty` 的最小可用命令；
  3) 在 Windows 侧脚手架一个最小 FastAPI 服务（仅 `POST /jobs` + 单线程处理）。
- 涉及依赖安装（PyTorch/Real-ESRGAN/ONNXRuntime）前会先给出清单与影响评估，并征求你的同意。
