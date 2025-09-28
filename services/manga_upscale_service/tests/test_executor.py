"""Tests for the Real-ESRGAN job execution pipeline."""

from __future__ import annotations

import asyncio
import json
import zipfile
from pathlib import Path
from typing import Callable

import cv2
import numpy as np
import pytest

from manga_upscale_service import executor
from main import JobCreate, JobInput, JobParams, JobRecord


class DummyEngine(executor.UpscaleEngineProtocol):
    """Stub upscaler that only doubles image size using nearest interpolation."""

    def __init__(self) -> None:
        self.calls: list[Path] = []

    def enhance(self, image: np.ndarray, outscale: float) -> np.ndarray:
        self.calls.append(Path(""))
        height, width = image.shape[:2]
        return cv2.resize(image, (int(width * outscale), int(height * outscale)), interpolation=cv2.INTER_NEAREST)


def _make_image(path: Path, color: tuple[int, int, int]) -> None:
    data = np.zeros((8, 8, 3), dtype=np.uint8)
    data[:, :] = np.array(color, dtype=np.uint8)
    cv2.imwrite(str(path), data)


def _prepare_storage(root: Path) -> executor.ServicePaths:
    incoming = root / "incoming"
    staging = root / "staging"
    outputs = root / "outputs"
    artifacts = root / "artifacts"
    models = root / "models"

    for folder in (incoming, staging, outputs, artifacts, models):
        folder.mkdir(parents=True, exist_ok=True)

    return executor.ServicePaths(
        storage_root=root,
        incoming_dir=incoming,
        staging_dir=staging,
        outputs_dir=outputs,
        artifacts_dir=artifacts,
        models_dir=models,
    )


def _run_executor(
    job_id: str,
    payload: JobCreate,
    paths: executor.ServicePaths,
    *,
    total_expected: int,
) -> executor.JobExecutionResult:
    record = JobRecord(payload=payload)
    engine = DummyEngine()

    progress: list[int] = []

    def callback(processed: int, total: int) -> None:
        assert total == total_expected
        progress.append(processed)

    async def run() -> executor.JobExecutionResult:
        return await executor.execute_job(
            job_id,
            record,
            paths,
            engine_factory=lambda _params, _device: engine,
            progress_callback=callback,
        )

    result = asyncio.run(run())
    assert progress[-1] == total_expected
    return result


def test_execute_folder_input_creates_artifact(tmp_path: Path) -> None:
    paths = _prepare_storage(tmp_path)

    incoming_dir = paths.incoming_dir / "demo"
    incoming_dir.mkdir(parents=True, exist_ok=True)

    _make_image(incoming_dir / "0001.jpg", (255, 0, 0))
    _make_image(incoming_dir / "0002.jpg", (0, 255, 0))

    payload = JobCreate(
        title="My Manga",
        volume="Vol1",
        input=JobInput(type="folder", path="demo"),
        params=JobParams(),
    )

    result = _run_executor("job-folder", payload, paths, total_expected=2)

    artifact_path = paths.storage_root / result.artifact_path
    assert artifact_path.exists()

    with zipfile.ZipFile(artifact_path) as archive:
        names = set(archive.namelist())
        assert "0001.jpg" in names
        assert "0002.jpg" in names
        assert "artifact-report.json" in names
        manifest = json.loads(archive.read("artifact-report.json"))
        assert manifest["summary"]["processed"] == 2


def test_execute_zip_input_unpacks_then_processes(tmp_path: Path) -> None:
    paths = _prepare_storage(tmp_path)

    zip_name = paths.incoming_dir / "sample.zip"
    with zipfile.ZipFile(zip_name, "w") as archive:
        for idx, color in enumerate(((0, 0, 255), (255, 255, 0)), start=1):
            filename = f"{idx:04d}.jpg"
            data = np.zeros((4, 4, 3), dtype=np.uint8)
            data[:, :] = np.array(color, dtype=np.uint8)
            archive.writestr(filename, cv2.imencode(".jpg", data)[1].tobytes())

    payload = JobCreate(
        title="Zip Manga",
        volume="V2",
        input=JobInput(type="zip", path="sample.zip"),
        params=JobParams(),
    )

    result = _run_executor("job-zip", payload, paths, total_expected=2)

    artifact_path = paths.storage_root / result.artifact_path
    assert artifact_path.exists()

    with zipfile.ZipFile(artifact_path) as archive:
        assert {name for name in archive.namelist() if name.endswith(".jpg")} == {"0001.jpg", "0002.jpg"}


def test_anime_model_definition_uses_six_blocks() -> None:
    definition = executor.MODEL_DEFINITIONS["realesrgan_x4plus_anime_6b"]
    assert definition.network_args["num_block"] == 6
