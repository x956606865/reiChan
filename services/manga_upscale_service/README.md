# Manga Upscale Service (Prototype)

本目录提供 Windows 端 FastAPI 服务的 M1 雏形，实现以下能力：

- `POST /jobs`：接受 Copyparty 上传的目录或 zip，记录请求并异步模拟处理流程；
- `GET /jobs/{job_id}`：查询作业状态、进度与最终产物路径；
- `GET /jobs/{job_id}/artifact`：在作业完成后返回压缩包的预期生成位置；
- `GET /health` / `GET /models`：提供基础探活与模型枚举。

当前实现仅使用 `asyncio.sleep` 模拟 GPU 推理；后续里程碑会接入 Real-ESRGAN、显卡设备检测与真实的作业队列。

## 运行方式

```bash
python -m venv .venv
.\.venv\Scripts\activate
pip install -e .[dev]
uvicorn main:app --host 0.0.0.0 --port 8001 --reload
```

启动后可通过 `http://localhost:8001/docs` 访问自动生成的交互文档。

## 后续扩展点

- 将 `simulate_execution` 替换为真实的推理调用（PyTorch / ONNXRuntime / realesrgan-ncnn-vulkan）；
- 引入持久化作业队列（SQLite / Redis）与磁盘容量管理；
- 增加 WebSocket 推送，实现 Tauri 前端无轮询的数据更新；
- 接入完整的 Copyparty storage root 配置以及路径安全校验。
