"""FastAPI backend for the Manga Upscale agent powered by Real-ESRGAN."""

from __future__ import annotations

import asyncio
import os
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Literal, Optional, Set

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect, Request, Response
from fastapi.responses import FileResponse
from pydantic import BaseModel, ConfigDict, Field

from executor import (
    MODEL_DEFINITIONS,
    JobExecutionResult,
    ServicePaths,
    execute_job,
    default_engine_factory,
)

JobStatus = Literal["PENDING", "RUNNING", "SUCCESS", "FAILED"]


def _to_camel(string: str) -> str:
    """Convert snake_case field names to camelCase for API I/O."""

    head, *tail = string.split("_")
    return head + "".join(part.capitalize() for part in tail)


class ApiModel(BaseModel):
    """Base model that exposes camelCase aliases to match the UI contract."""

    model_config = ConfigDict(
        alias_generator=_to_camel,
        populate_by_name=True,
        serialize_by_alias=True,
    )


class JobInput(ApiModel):
    """Describe the uploaded asset that should be processed."""

    type: Literal["folder", "zip"] = Field(default="folder", description="Source payload kind")
    path: str = Field(..., min_length=1, description="Relative path inside storage root")


class JobParams(ApiModel):
    """Inference parameters (fixed defaults for M1)."""

    scale: int = Field(default=2, ge=1, le=4, description="Upscale factor")
    model: str = Field(
        default="RealESRGAN_x4plus_anime_6B",
        description="Model identifier to load on the worker",
    )
    denoise: Literal["low", "medium", "high"] = Field(default="medium")
    output_format: Literal["jpg", "png", "webp"] = Field(default="jpg")
    jpeg_quality: int = Field(default=95, ge=1, le=100)
    tile_size: Optional[int] = Field(default=None, ge=32, le=1024)
    tile_pad: Optional[int] = Field(default=None, ge=0, le=128)
    batch_size: Optional[int] = Field(default=None, ge=1, le=16)
    device: Literal["auto", "cuda", "cpu"] = Field(default="auto")


class JobCreate(ApiModel):
    """Request body for ``POST /jobs``."""

    title: str = Field(..., min_length=1)
    volume: str = Field(..., min_length=1)
    input: JobInput
    params: JobParams = JobParams()


class JobSubmitted(ApiModel):
    job_id: str


class JobResumePayload(ApiModel):
    inputPath: Optional[str] = None
    inputType: Optional[str] = None


class JobState(ApiModel):
    job_id: str
    status: JobStatus
    processed: int
    total: int
    artifact_path: Optional[str] = None
    message: Optional[str] = None
    retries: int = 0
    last_error: Optional[str] = None
    artifact_hash: Optional[str] = None
    params: Optional[JobParams] = None
    metadata: Optional[dict[str, Optional[str]]] = None


@dataclass
class JobRecord:
    payload: JobCreate
    status: JobStatus = "PENDING"
    processed: int = 0
    total: int = 0
    artifact_path: Optional[str] = None
    message: Optional[str] = None
    retries: int = 0
    last_error: Optional[str] = None
    artifact_hash: Optional[str] = None
    report_path: Optional[Path] = None


app = FastAPI(title="Manga Upscale Service", version="0.1.0")

jobs: Dict[str, JobRecord] = {}
jobs_lock = asyncio.Lock()
STORAGE_ROOT = Path(os.getenv("REICHAN_STORAGE_ROOT", "./storage")).resolve()
INCOMING_DIR = STORAGE_ROOT / "incoming"
STAGING_DIR = STORAGE_ROOT / "staging"
OUTPUTS_DIR = STORAGE_ROOT / "outputs"
ARTIFACTS_DIR = STORAGE_ROOT / "artifacts"
MODEL_ROOT = Path(os.getenv("REICHAN_MODEL_ROOT", STORAGE_ROOT / "models")).resolve()

for directory in (STORAGE_ROOT, INCOMING_DIR, STAGING_DIR, OUTPUTS_DIR, ARTIFACTS_DIR, MODEL_ROOT):
    directory.mkdir(parents=True, exist_ok=True)

SERVICE_PATHS = ServicePaths(
    storage_root=STORAGE_ROOT,
    incoming_dir=INCOMING_DIR,
    staging_dir=STAGING_DIR,
    outputs_dir=OUTPUTS_DIR,
    artifacts_dir=ARTIFACTS_DIR,
    models_dir=MODEL_ROOT,
)

MAX_CONCURRENCY = max(1, int(os.getenv("REICHAN_MAX_CONCURRENCY", "1")))
worker_semaphore = asyncio.Semaphore(MAX_CONCURRENCY)

JobQueue = asyncio.Queue
job_subscribers: Dict[str, Set[JobQueue]] = {}


@app.get("/health")
async def healthcheck() -> dict[str, object]:
    async with jobs_lock:
        active = sum(1 for record in jobs.values() if record.status == "RUNNING")
    return {
        "status": "ok",
        "active_jobs": active,
        "registered_jobs": len(jobs),
        "default_model": "RealESRGAN_x4plus_anime_6B",
        "max_concurrency": MAX_CONCURRENCY,
        "storage_root": str(STORAGE_ROOT),
    }


@app.get("/models")
async def list_models() -> dict[str, list[dict[str, object]]]:
    models: list[dict[str, object]] = []
    for definition in MODEL_DEFINITIONS.values():
        models.append(
            {
                "name": definition.name,
                "scale": definition.scale,
                "recommended_scale": definition.default_outscale,
                "weights": definition.weights,
                "download_url": definition.download_url,
            }
        )
    return {"models": models}


@app.post("/jobs", response_model=JobSubmitted, status_code=202)
async def create_job(payload: JobCreate) -> JobSubmitted:
    job_id = str(uuid.uuid4())
    record = JobRecord(payload=payload, total=_estimate_total_frames(payload))
    async with jobs_lock:
        jobs[job_id] = record

    await _broadcast(job_id)
    asyncio.create_task(_run_job(job_id))
    return JobSubmitted(job_id=job_id)


@app.get("/jobs/{job_id}", response_model=JobState)
async def get_job(job_id: str) -> JobState:
    state = await _snapshot(job_id)

    if state is None:
        raise HTTPException(status_code=404, detail="Job not found")

    return state


@app.post("/jobs/{job_id}/resume", response_model=JobState)
async def resume_job(job_id: str, payload: JobResumePayload | None = None) -> JobState:
    async with jobs_lock:
        record = jobs.get(job_id)

        if record is None:
            raise HTTPException(status_code=404, detail="Job not found")

        if record.status == "RUNNING":
            return _build_state(job_id, record)

        record.retries += 1
        record.status = "PENDING"
        record.message = None
        record.last_error = None
        record.processed = 0
        record.total = 0
        record.artifact_path = None
        record.artifact_hash = None
        record.report_path = None

        if payload and payload.inputPath:
            record.payload.input.path = payload.inputPath
        if payload and payload.inputType:
            record.payload.input.type = payload.inputType

    await _broadcast(job_id)
    asyncio.create_task(_run_job(job_id))
    state = await _snapshot(job_id)
    assert state is not None
    return state


@app.post("/jobs/{job_id}/cancel", response_model=JobState)
async def cancel_job(job_id: str) -> JobState:
    async with jobs_lock:
        record = jobs.get(job_id)

        if record is None:
            raise HTTPException(status_code=404, detail="Job not found")

        record.status = "FAILED"
        record.message = "Cancelled by user"
        record.last_error = record.message

    await _broadcast(job_id)
    state = await _snapshot(job_id)
    assert state is not None
    return state


@app.get("/jobs/{job_id}/artifact")
async def get_artifact(job_id: str, request: Request):
    async with jobs_lock:
        record = jobs.get(job_id)

    if record is None:
        raise HTTPException(status_code=404, detail="Job not found")

    if record.artifact_path is None:
        raise HTTPException(status_code=409, detail="Job not finished yet")

    artifact_path = (STORAGE_ROOT / record.artifact_path).resolve()
    try:
        artifact_path.relative_to(ARTIFACTS_DIR)
    except ValueError as exc:
        raise HTTPException(status_code=500, detail="Artifact path escaped storage root") from exc

    if not artifact_path.exists():
        raise HTTPException(status_code=410, detail="Artifact missing")

    etag = record.artifact_hash or _hash_file(artifact_path)

    if_none_match = request.headers.get("if-none-match")
    if if_none_match and etag and if_none_match == etag:
        return Response(status_code=304)

    record.artifact_hash = etag

    headers = {"ETag": etag} if etag else None
    return FileResponse(
        artifact_path,
        media_type="application/zip",
        filename=artifact_path.name,
        headers=headers,
    )


@app.get("/jobs/{job_id}/report")
async def get_report(job_id: str) -> dict[str, object]:
    async with jobs_lock:
        record = jobs.get(job_id)

    if record is None:
        raise HTTPException(status_code=404, detail="Job not found")

    return {
        "jobId": job_id,
        "status": record.status,
        "artifactPath": record.artifact_path,
        "artifactHash": record.artifact_hash,
        "retries": record.retries,
        "lastError": record.last_error,
        "message": record.message,
    }


@app.websocket("/ws/jobs/{job_id}")
async def job_events(websocket: WebSocket, job_id: str) -> None:
    await websocket.accept()
    queue: JobQueue = JobQueue()

    try:
        initial = await _register_listener(job_id, queue)
    except KeyError:
        await websocket.close(code=4404)
        return

    await websocket.send_json(initial)

    try:
        while True:
            event = await queue.get()
            await websocket.send_json(event)
    except WebSocketDisconnect:
        pass
    finally:
        await _remove_listener(job_id, queue)


async def _run_job(job_id: str) -> None:
    loop = asyncio.get_running_loop()
    try:
        async with worker_semaphore:
            record = await _mark_running(job_id)
            if record is None:
                return

            def progress(processed: int, total: int) -> None:
                asyncio.run_coroutine_threadsafe(
                    _update_progress(job_id, processed, total), loop
                )

            result = await execute_job(
                job_id,
                record,
                SERVICE_PATHS,
                engine_factory=default_engine_factory,
                progress_callback=progress,
            )

            await _store_success(job_id, result)
    except Exception as exc:  # pragma: no cover - defensive
        await _mark_failed(job_id, str(exc))


async def _mark_running(job_id: str) -> Optional[JobRecord]:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record is None:
            return None
        record.status = "RUNNING"
        record.total = 0
        record.message = None
        record.last_error = None
        record.processed = 0
        record.artifact_path = None
        record.artifact_hash = None
        record.report_path = None
    await _broadcast(job_id)
    return record


async def _store_success(job_id: str, result: JobExecutionResult) -> None:
    try:
        artifact_relative = result.artifact_path.resolve().relative_to(STORAGE_ROOT).as_posix()
    except ValueError as exc:  # pragma: no cover - defensive
        raise RuntimeError("Artifact escaped storage root") from exc

    try:
        report_relative = result.report_path.resolve().relative_to(STORAGE_ROOT)
    except ValueError:
        report_relative = None

    async with jobs_lock:
        record = jobs.get(job_id)
        if record:
            record.status = "SUCCESS"
            record.processed = result.processed
            record.total = result.total
            record.artifact_path = artifact_relative
            record.artifact_hash = result.artifact_hash
            record.message = None
            record.last_error = None
            record.report_path = report_relative
    await _broadcast(job_id)


async def _mark_failed(job_id: str, message: str) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record:
            record.status = "FAILED"
            record.message = message
            record.last_error = message
    await _broadcast(job_id)


async def _update_progress(job_id: str, processed: int, total: Optional[int] = None) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record:
            record.processed = processed
            if total is not None:
                record.total = total
    await _broadcast(job_id)


async def _snapshot(job_id: str) -> Optional[JobState]:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record is None:
            return None
        return _build_state(job_id, record)


async def _register_listener(job_id: str, queue: JobQueue) -> dict[str, object]:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record is None:
            raise KeyError(job_id)
        job_subscribers.setdefault(job_id, set()).add(queue)
        state = _build_state(job_id, record)
    return state.model_dump()


async def _remove_listener(job_id: str, queue: JobQueue) -> None:
    async with jobs_lock:
        listeners = job_subscribers.get(job_id)
        if not listeners:
            return
        listeners.discard(queue)
        if not listeners:
            job_subscribers.pop(job_id, None)


async def _broadcast(job_id: str) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record is None:
            return
        payload = _build_state(job_id, record).model_dump()
        listeners = list(job_subscribers.get(job_id, set()))

    for queue in listeners:
        try:
            queue.put_nowait(payload)
        except asyncio.QueueFull:  # pragma: no cover - defensive
            continue


def _build_state(job_id: str, record: JobRecord) -> JobState:
    return JobState(
        job_id=job_id,
        status=record.status,
        processed=record.processed,
        total=record.total,
        artifact_path=record.artifact_path,
        message=record.message,
        retries=record.retries,
        last_error=record.last_error,
        artifact_hash=record.artifact_hash,
        params=record.payload.params,
        metadata={
            "title": record.payload.title,
            "volume": record.payload.volume,
        },
    )
def _estimate_total_frames(payload: JobCreate) -> int:
    if payload.input.type == "zip":
        return 24
    return 48


if __name__ == "__main__":  # pragma: no cover
    import uvicorn

    uvicorn.run("main:app", host="0.0.0.0", port=8001, reload=True)
