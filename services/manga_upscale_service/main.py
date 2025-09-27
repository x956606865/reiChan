"""FastAPI backend prototype for the Manga Upscale agent (M1).

This service accepts folder inputs coming from Copyparty, enqueues a mock
Real-ESRGAN job, and streams progress updates via polling endpoints.

The implementation intentionally keeps the processing step lightweight: it
simulates GPU work with asyncio sleeps and prepares deterministic metadata for
the artifact path. Real model invocation (PyTorch / ONNX / NCNN) will replace
``simulate_execution`` in later milestones.
"""

from __future__ import annotations

import asyncio
import os
import uuid
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Literal, Optional, Set

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse
from pydantic import BaseModel, Field

JobStatus = Literal["PENDING", "RUNNING", "SUCCESS", "FAILED"]


class JobInput(BaseModel):
    """Describe the uploaded asset that should be processed."""

    type: Literal["folder", "zip"] = Field(default="folder", description="Source payload kind")
    path: str = Field(..., min_length=1, description="Relative path inside storage root")


class JobParams(BaseModel):
    """Inference parameters (fixed defaults for M1)."""

    scale: int = Field(default=2, ge=1, le=4, description="Upscale factor")
    model: str = Field(
        default="RealESRGAN_x4plus_anime_6B",
        description="Model identifier to load on the worker",
    )
    denoise: Literal["low", "medium", "high"] = Field(default="medium")


class JobCreate(BaseModel):
    """Request body for ``POST /jobs``."""

    title: str = Field(..., min_length=1)
    volume: str = Field(..., min_length=1)
    input: JobInput
    params: JobParams = JobParams()


class JobSubmitted(BaseModel):
    job_id: str


class JobState(BaseModel):
    job_id: str
    status: JobStatus
    processed: int
    total: int
    artifact_path: Optional[str] = None
    message: Optional[str] = None


@dataclass
class JobRecord:
    payload: JobCreate
    status: JobStatus = "PENDING"
    processed: int = 0
    total: int = 0
    artifact_path: Optional[str] = None
    message: Optional[str] = None


app = FastAPI(title="Manga Upscale Service", version="0.1.0")

jobs: Dict[str, JobRecord] = {}
jobs_lock = asyncio.Lock()
STORAGE_ROOT = Path(os.getenv("REICHAN_STORAGE_ROOT", "./storage")).resolve()
INCOMING_DIR = STORAGE_ROOT / "incoming"
STAGING_DIR = STORAGE_ROOT / "staging"
OUTPUTS_DIR = STORAGE_ROOT / "outputs"
ARTIFACTS_DIR = STORAGE_ROOT / "artifacts"

for directory in (STORAGE_ROOT, INCOMING_DIR, STAGING_DIR, OUTPUTS_DIR, ARTIFACTS_DIR):
    directory.mkdir(parents=True, exist_ok=True)

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
    return {
        "models": [
            {
                "name": "RealESRGAN_x4plus_anime_6B",
                "scale": 4,
                "recommended_scale": 2,
                "notes": "Anime-focused Real-ESRGAN weights",
            }
        ]
    }


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


@app.get("/jobs/{job_id}/artifact")
async def get_artifact(job_id: str) -> FileResponse:
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

    return FileResponse(
        artifact_path,
        media_type="application/zip",
        filename=artifact_path.name,
    )


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
    try:
        async with worker_semaphore:
            await _mark_running(job_id)

            total = await _job_total(job_id)
            for step in range(1, total + 1):
                await simulate_execution(job_id, step)

            await _mark_success(job_id)
    except Exception as exc:  # pragma: no cover - defensive
        await _mark_failed(job_id, str(exc))


async def simulate_execution(job_id: str, step: int) -> None:
    await asyncio.sleep(0.4)
    await _update_progress(job_id, step)


async def _mark_running(job_id: str) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record is None:
            raise RuntimeError(f"Job {job_id} disappeared")
        record.status = "RUNNING"
        record.total = max(record.total, 8)
    await _broadcast(job_id)

async def _job_total(job_id: str) -> int:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record is None:
            raise RuntimeError(f"Job {job_id} disappeared")
        return record.total


async def _mark_success(job_id: str) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record:
            record.status = "SUCCESS"
            record.processed = record.total
            record.artifact_path = _ensure_artifact(record.payload, job_id, record.total)
            record.message = None
    await _broadcast(job_id)


async def _mark_failed(job_id: str, message: str) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record:
            record.status = "FAILED"
            record.message = message
    await _broadcast(job_id)


async def _update_progress(job_id: str, processed: int) -> None:
    async with jobs_lock:
        record = jobs.get(job_id)
        if record:
            record.processed = processed
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
    )


def _ensure_artifact(payload: JobCreate, job_id: str, processed: int) -> str:
    artifact_name = f"{_slug(payload.title)}-{_slug(payload.volume)}-{job_id}.zip"
    artifact_path = ARTIFACTS_DIR / artifact_name

    if not artifact_path.exists():
        with zipfile.ZipFile(artifact_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            summary = (
                f"Job {job_id}\n"
                f"Title: {payload.title}\n"
                f"Volume: {payload.volume}\n"
                f"Processed frames: {processed}\n"
                "This is a placeholder artifact generated during M2.\n"
            )
            archive.writestr("SUMMARY.txt", summary)

    return artifact_path.relative_to(STORAGE_ROOT).as_posix()


def _slug(value: str) -> str:
    safe = value.strip().lower().replace(" ", "-")
    return "".join(ch for ch in safe if ch.isalnum() or ch in {"-", "_"}) or "unknown"


def _estimate_total_frames(payload: JobCreate) -> int:
    if payload.input.type == "zip":
        return 24
    return 48


if __name__ == "__main__":  # pragma: no cover
    import uvicorn

    uvicorn.run("main:app", host="0.0.0.0", port=8001, reload=True)
