# Manga Upscale Service

基于 FastAPI + PyTorch 的漫画高清化后端，现已接入真实的 [Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) 动漫模型推理流程，并对 Tauri 前端提供完整的作业生命周期管理（提交 / 进度 / Resume / Cancel / Artifact 下载与哈希校验）。

## 核心能力

- `POST /jobs`：接受 Copyparty 上传的目录或 zip，落盘到 staging，并异步运行 Real-ESRGAN 推理。
- `GET /jobs/{job_id}`：返回实时进度、最终产物相对路径与 SHA-256。
- `WS /ws/jobs/{job_id}`：提供推送式作业事件。
- `GET /jobs/{job_id}/artifact`：输出打包后的结果 zip（含 `artifact-report.json` 与推理图片）。
- `GET /models`：枚举当前可用的模型及对应权重文件。

作业完成后，服务会在 `outputs/<title>/<volume>/<job_id>/` 产出推理文件，并在 `artifacts/` 下生成压缩包供前端下载对账。

## 环境准备

1. **Python 依赖**

   ```bash
   cd services/manga_upscale_service
   python -m venv .venv
   source .venv/bin/activate   # PowerShell 使用 .\.venv\Scripts\Activate.ps1
   pip install -e .[dev]
   ```

   > ⚠️ 本项目依赖 `torch`、`torchvision`、`torchaudio`、`realesrgan`、`opencv-python-headless` 等较大的包，请确保机器具备足够磁盘与网络带宽。若需 GPU 推理，请先根据显卡驱动选择合适的 CUDA 发行（如 CUDA 12.6 对应的命令：`pip install torch==2.6.0 torchvision==0.21.0 torchaudio==2.6.0 --index-url https://download.pytorch.org/whl/cu126`），再执行 `pip install -e .`。
   > ℹ️ 为规避官方 `basicsr` 1.4.2 缺失 `basicsr.version` 的已知 bug，我们保留 `basicsr` 的版本上限并额外引入 wheel 兼容发行 `my-basicsr==1.4.2`，无需额外操作即可正常导入。

2. **模型权重**

   将下列权重文件下载至 `storage/models/`（可通过环境变量 `REICHAN_MODEL_ROOT` 指定其他目录）：

   | 模型 Key | 默认输出倍率 | 权重文件 | 下载地址 |
   | --- | --- | --- | --- |
   | `RealESRGAN_x4plus_anime_6B` | 推荐 outscale=2 | `RealESRGAN_x4plus_anime_6B.pth` | <https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.2.4/RealESRGAN_x4plus_anime_6B.pth> |
   | `realesr-animevideov3` | 推荐 outscale=2 | `realesr-animevideov3.pth` | <https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesr-animevideov3.pth> |

   权重路径允许放置在存储根以外的位置，只需通过 `REICHAN_MODEL_ROOT` 指向该目录即可。

3. **目录结构**

   服务默认在 `storage/` 下维护以下子目录：

   - `incoming/`：Copyparty 上传的源素材（zip 或文件夹）；
   - `staging/`：作业临时解压 / 拷贝空间（作业完成后自动清理）；
   - `outputs/`：推理结果（`<title>/<volume>/<job_id>/`）；
   - `artifacts/`：供前端下载的 zip；
   - `models/`：缺省权重目录（可被 `REICHAN_MODEL_ROOT` 覆盖）。

## 启动服务

```bash
uvicorn main:app --host 0.0.0.0 --port 8001 --reload
```

启动后可访问 `http://localhost:8001/docs` 查看 OpenAPI UI。Windows 用户可直接运行同目录下的 `start_service.bat`。

## 作业执行细节

- 自动识别输入类型：
  - `folder`：深拷贝到 staging；
  - `zip`：安全解压（阻止目录穿越）。
- 支持图片格式：`.jpg/.jpeg/.png/.webp/.bmp`。
- 推理过程中实时更新进度至 WebSocket 和 REST 轮询接口。
- 产物包含：
  - 推理后的图片（文件名沿用输入文件并按 UI 选择的输出格式写出）；
  - `artifact-report.json`：列出每个文件的 SHA-256、字节数、设备信息、模型与倍率摘要。

## 配置项

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `REICHAN_STORAGE_ROOT` | `./storage` | 存储根目录 |
| `REICHAN_MODEL_ROOT` | `<storage>/models` | 模型权重目录 |
| `REICHAN_MAX_CONCURRENCY` | `1` | 同时运行的作业数量 |

## 测试

该目录新增了针对推理管线的单元测试，可使用下列命令运行：

```bash
python -m pytest
```

> 如环境尚未安装 `pytest`，请先执行 `pip install pytest`（需预先征得批准）。

## 后续规划

- 扩展更多模型（RealESRGAN 通用模型 / 自训练权重）。
- 增加错误重试、分片推理与 GPU 内存自适应 tile 策略。
- 与 Copyparty 上传模块联动生成 manifest，用于端到端对账。
