"""Real-ESRGAN execution pipeline for the Manga Upscale service."""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import shutil
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Optional, Protocol, Sequence, TYPE_CHECKING

import cv2
import numpy as np

try:  # pragma: no cover - import style depends on execution context
    from ._compat import ensure_basicsr_version_module
except ImportError:  # pragma: no cover - allow running as a script
    from _compat import ensure_basicsr_version_module

ensure_basicsr_version_module()

from basicsr.archs.rrdbnet_arch import RRDBNet
from realesrgan import RealESRGANer

if TYPE_CHECKING:  # pragma: no cover - typing only
    from main import JobRecord
    from main import JobParams


LOGGER = logging.getLogger(__name__)


try:  # pragma: no cover - optional dependency on CUDA runtime
    import torch
except Exception:  # pragma: no cover - torch not installed or CUDA unavailable
    torch = None


class UpscaleEngineProtocol(Protocol):
    """Minimal protocol implemented by the Real-ESRGAN upscaler implementation."""

    def enhance(self, image: np.ndarray, outscale: float) -> np.ndarray:
        """Upscale ``image`` and return the enhanced frame."""


EngineFactory = Callable[["JobParams", str, Path], UpscaleEngineProtocol]
ProgressCallback = Callable[[int, int], None]


@dataclass(frozen=True)
class ServicePaths:
    """Resolved directories used by the service."""

    storage_root: Path
    incoming_dir: Path
    staging_dir: Path
    outputs_dir: Path
    artifacts_dir: Path
    models_dir: Path


@dataclass(frozen=True)
class ModelDefinition:
    """Describe a supported Real-ESRGAN model."""

    name: str
    weights: str
    scale: int
    network_args: dict[str, int]
    default_outscale: float
    download_url: str


MODEL_DEFINITIONS: dict[str, ModelDefinition] = {
    "realesrgan_x4plus_anime_6b": ModelDefinition(
        name="RealESRGAN_x4plus_anime_6B",
        weights="RealESRGAN_x4plus_anime_6B.pth",
        scale=4,
        network_args={
            "num_in_ch": 3,
            "num_out_ch": 3,
            "num_feat": 64,
            "num_block": 6,
            "num_grow_ch": 32,
            "scale": 4,
        },
        default_outscale=2.0,
        download_url="https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.2.4/RealESRGAN_x4plus_anime_6B.pth",
    ),
    "realesr_animevideov3": ModelDefinition(
        name="realesr-animevideov3",
        weights="realesr-animevideov3.pth",
        scale=4,
        network_args={
            "num_in_ch": 3,
            "num_out_ch": 3,
            "num_feat": 64,
            "num_block": 23,
            "num_grow_ch": 32,
            "scale": 4,
        },
        default_outscale=2.0,
        download_url="https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesr-animevideov3.pth",
    ),
}


MODEL_ALIASES = {
    "realesrgan_x4plus_anime_6b": "realesrgan_x4plus_anime_6b",
    "realesrgan_x4plus_anime6b": "realesrgan_x4plus_anime_6b",
    "RealESRGAN_x4plus_anime_6B": "realesrgan_x4plus_anime_6b",
    "reaLesrgan_x4plus_anime_6b": "realesrgan_x4plus_anime_6b",
    "real-esrgan-anime": "realesrgan_x4plus_anime_6b",
    "real-esrgan-anima": "realesrgan_x4plus_anime_6b",
    "realesr-animevideov3": "realesr_animevideov3",
}


@dataclass
class JobExecutionResult:
    processed: int
    total: int
    output_dir: Path
    artifact_path: Path
    artifact_hash: str
    report_path: Path


def default_engine_factory(params: "JobParams", device: str, models_dir: Path) -> UpscaleEngineProtocol:
    """Instantiate the canonical Real-ESRGAN engine for the requested model."""

    model_key = MODEL_ALIASES.get(params.model, params.model)
    definition = MODEL_DEFINITIONS.get(model_key)

    if definition is None:
        raise ValueError(f"Unsupported Real-ESRGAN model '{params.model}'")

    weights_path = (models_dir / definition.weights).resolve()

    if not weights_path.exists():
        raise FileNotFoundError(
            f"Missing weights file '{weights_path}'. Download it from {definition.download_url} "
            "or update REICHAN_MODEL_ROOT to point to the directory containing the weights."
        )

    half_precision = device.startswith("cuda") and (params.device != "cpu")

    network = RRDBNet(**definition.network_args)

    engine = RealESRGANer(
        scale=definition.scale,
        model_path=str(weights_path),
        model=network,
        tile=params.tile_size or 0,
        tile_pad=params.tile_pad or 10,
        pre_pad=0,
        half=half_precision,
        device=device,
    )

    return _RealEsrganWrapper(engine)


class _RealEsrganWrapper(UpscaleEngineProtocol):
    """Adapter that normalises the ``RealESRGANer`` return signature."""

    def __init__(self, engine: RealESRGANer) -> None:
        self._engine = engine

    def enhance(self, image: np.ndarray, outscale: float) -> np.ndarray:
        restored, _ = self._engine.enhance(image, outscale=outscale)
        return restored


async def execute_job(
    job_id: str,
    record: "JobRecord",
    paths: ServicePaths,
    *,
    engine_factory: EngineFactory = default_engine_factory,
    progress_callback: Optional[ProgressCallback] = None,
) -> JobExecutionResult:
    """Execute a Real-ESRGAN job in a worker thread."""

    loop = asyncio.get_running_loop()

    def notify(processed: int, total: int) -> None:
        if not progress_callback:
            return
        progress_callback(processed, total)

    return await asyncio.to_thread(
        _execute_sync,
        job_id,
        record,
        paths,
        engine_factory,
        notify,
    )


def _execute_sync(
    job_id: str,
    record: "JobRecord",
    paths: ServicePaths,
    engine_factory: EngineFactory,
    progress_callback: ProgressCallback,
) -> JobExecutionResult:
    payload = record.payload

    source = _locate_source(paths, payload.input.path)
    stage_root = paths.staging_dir / job_id
    input_dir = stage_root / "input"
    output_dir = stage_root / "output"

    if stage_root.exists():
        shutil.rmtree(stage_root)
    input_dir.mkdir(parents=True, exist_ok=True)
    output_dir.mkdir(parents=True, exist_ok=True)

    _materialise_input(payload.input.type, source, input_dir)

    image_paths = _gather_images(input_dir)
    if not image_paths:
        raise ValueError("No images found in staged payload")

    total = len(image_paths)
    progress_callback(0, total)

    device = _select_device(payload.params)
    engine = engine_factory(payload.params, device, paths.models_dir)

    artifact_output_dir = paths.outputs_dir / _slug(payload.title) / _slug(payload.volume) / job_id
    if artifact_output_dir.exists():
        shutil.rmtree(artifact_output_dir)
    artifact_output_dir.mkdir(parents=True, exist_ok=True)

    processed = 0
    manifest_items: list[dict[str, object]] = []

    model_key = MODEL_ALIASES.get(payload.params.model, payload.params.model)
    definition = MODEL_DEFINITIONS.get(model_key)
    if definition is None:
        raise ValueError(f"Unsupported Real-ESRGAN model '{payload.params.model}'")

    outscale = float(payload.params.scale or definition.default_outscale)
    dst_ext = _determine_extension(payload.params.output_format)

    for image_path in image_paths:
        decode_started = time.perf_counter()
        image = cv2.imdecode(np.fromfile(str(image_path), dtype=np.uint8), cv2.IMREAD_UNCHANGED)
        decode_elapsed = time.perf_counter() - decode_started
        if image is None:
            raise ValueError(f"Unable to decode image '{image_path}'")

        tile_size, tile_pad = _resolve_tile_settings(image, payload.params, definition, device)
        if hasattr(engine, "tile") and engine.tile != tile_size:
            engine.tile = tile_size
        if hasattr(engine, "tile_pad") and tile_pad is not None and engine.tile_pad != tile_pad:
            engine.tile_pad = tile_pad

        cuda_metrics_enabled = _cuda_metrics_available(device)
        if cuda_metrics_enabled:
            torch.cuda.reset_peak_memory_stats()

        enhance_started = time.perf_counter()
        restored = engine.enhance(image, outscale=outscale)
        enhance_elapsed = time.perf_counter() - enhance_started

        peak_memory = None
        if cuda_metrics_enabled:
            torch.cuda.synchronize()
            try:
                peak_memory = torch.cuda.max_memory_allocated()
            except RuntimeError:
                peak_memory = None

        output_name = image_path.stem + dst_ext
        output_path = artifact_output_dir / output_name

        _write_image(restored, output_path, payload.params)

        digest = _digest_file(output_path)
        manifest_items.append(
            {
                "filename": output_name,
                "sha256": digest["sha256"],
                "bytes": digest["bytes"],
            }
        )

        processed += 1
        progress_callback(processed, total)

        LOGGER.info(
            "Job %s: processed %s (%dx%d → %s, scale=%.2f, tile=%d, pad=%d) decode=%.3fs enhance=%.3fs peak=%.1f MiB",
            job_id,
            image_path.name,
            image.shape[1],
            image.shape[0],
            output_name,
            outscale,
            tile_size,
            tile_pad or 0,
            decode_elapsed,
            enhance_elapsed,
            _bytes_to_mebibytes(peak_memory),
        )

    report = _build_report(
        job_id=job_id,
        payload=payload,
        processed=processed,
        total=total,
        items=manifest_items,
        output_dir=artifact_output_dir,
        device=device,
        outscale=outscale,
    )

    report_path = artifact_output_dir / "artifact-report.json"
    report_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    artifact_path = paths.artifacts_dir / f"{_slug(payload.title)}-{_slug(payload.volume)}-{job_id}.zip"
    if artifact_path.exists():
        artifact_path.unlink()

    with zipfile.ZipFile(artifact_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for item in manifest_items:
            archive.write(artifact_output_dir / item["filename"], arcname=item["filename"])
        archive.write(report_path, arcname="artifact-report.json")

    artifact_hash = _hash_file(artifact_path)

    shutil.rmtree(stage_root)

    return JobExecutionResult(
        processed=processed,
        total=total,
        output_dir=artifact_output_dir,
        artifact_path=artifact_path,
        artifact_hash=artifact_hash,
        report_path=report_path,
    )


def _locate_source(paths: ServicePaths, relative_path: str) -> Path:
    candidate = (paths.storage_root / relative_path).resolve()
    try:
        candidate.relative_to(paths.storage_root)
    except ValueError:
        raise ValueError(f"Input path '{relative_path}' escapes the storage root")

    if candidate.exists():
        return candidate

    fallback = (paths.incoming_dir / relative_path).resolve()
    try:
        fallback.relative_to(paths.storage_root)
    except ValueError:
        raise ValueError(f"Input path '{relative_path}' escapes the storage root")

    if fallback.exists():
        return fallback

    raise FileNotFoundError(f"Input payload '{relative_path}' not found under storage root")


def _materialise_input(kind: str, source: Path, destination: Path) -> None:
    if kind == "folder":
        if not source.is_dir():
            raise ValueError(f"Expected directory input but got '{source}'")
        shutil.copytree(source, destination, dirs_exist_ok=True)
        return

    if kind == "zip":
        if not source.is_file():
            raise ValueError(f"Expected zip payload but got '{source}'")
        with zipfile.ZipFile(source) as archive:
            for member in archive.infolist():
                _safe_extract(archive, member, destination)
        return

    raise ValueError(f"Unsupported input type '{kind}'")


def _safe_extract(archive: zipfile.ZipFile, member: zipfile.ZipInfo, destination: Path) -> None:
    extracted_path = destination / member.filename
    resolved = extracted_path.resolve()
    try:
        resolved.relative_to(destination.resolve())
    except ValueError:
        raise ValueError(f"Zip entry '{member.filename}' escapes extraction directory")
    if member.is_dir():
        resolved.mkdir(parents=True, exist_ok=True)
    else:
        resolved.parent.mkdir(parents=True, exist_ok=True)
        with archive.open(member) as source, open(resolved, "wb") as target:
            shutil.copyfileobj(source, target)


def _gather_images(root: Path) -> list[Path]:
    exts = {".jpg", ".jpeg", ".png", ".webp", ".bmp"}
    files: list[Path] = []
    for entry in root.rglob("*"):
        if entry.is_file() and entry.suffix.lower() in exts:
            files.append(entry)
    files.sort()
    return files


def _determine_extension(output_format: str) -> str:
    mapping = {
        "jpg": ".jpg",
        "jpeg": ".jpg",
        "png": ".png",
        "webp": ".webp",
    }
    return mapping.get(output_format.lower(), ".jpg")


def _write_image(image: np.ndarray, path: Path, params: "JobParams") -> None:
    ext = path.suffix.lower()
    encode_ext = ".jpg"
    encode_params = [cv2.IMWRITE_JPEG_QUALITY, params.jpeg_quality]

    if ext == ".png":
        encode_ext = ".png"
        encode_params = [cv2.IMWRITE_PNG_COMPRESSION, 1]
    elif ext == ".webp":
        encode_ext = ".webp"
        encode_params = [cv2.IMWRITE_WEBP_QUALITY, params.jpeg_quality]
    elif ext in {".jpeg", ".jpg"}:
        encode_ext = ".jpg"

    # Encode in-memory so that the subsequent write uses Python IO, which tolerates Unicode paths on Windows.
    success, buffer = cv2.imencode(encode_ext, image, encode_params)
    if not success:
        raise RuntimeError(f"Failed to encode image '{path.name}' with format '{encode_ext}'")

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(buffer.tobytes())


def _digest_file(path: Path) -> dict[str, object]:
    sha = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(8192), b""):
            if not chunk:
                break
            sha.update(chunk)
            size += len(chunk)
    return {"sha256": sha.hexdigest(), "bytes": size}


def _hash_file(path: Path) -> str:
    return _digest_file(path)["sha256"]  # type: ignore[return-value]


def _build_report(
    *,
    job_id: str,
    payload,
    processed: int,
    total: int,
    items: Sequence[dict[str, object]],
    output_dir: Path,
    device: str,
    outscale: float,
) -> dict[str, object]:
    return {
        "jobId": job_id,
        "title": payload.title,
        "volume": payload.volume,
        "processed": processed,
        "total": total,
        "model": payload.params.model,
        "outscale": outscale,
        "device": device,
        "summary": {
            "processed": processed,
            "failed": total - processed,
        },
        "items": list(items),
        "outputDir": str(output_dir),
    }


def _resolve_tile_settings(
    image: np.ndarray,
    params: "JobParams",
    definition: ModelDefinition,
    device: str,
) -> tuple[int, int]:
    """Derive tile size/padding for the current frame, honouring user overrides."""

    if params.tile_size:
        return int(params.tile_size), int(params.tile_pad or 10)

    tile_pad = int(params.tile_pad or 10)

    if device != "cuda" or not _cuda_metrics_available(device):
        return 0, tile_pad

    height, width = image.shape[:2]
    outscale = float(params.scale or definition.default_outscale)
    scaled_area = (height * outscale) * (width * outscale)
    megapixels = scaled_area / 1_000_000

    if megapixels >= 32:
        return 256, tile_pad
    if megapixels >= 20:
        return 320, tile_pad
    if megapixels >= 14:
        return 384, tile_pad
    if megapixels >= 9:
        return 448, tile_pad
    return 0, tile_pad


def _cuda_metrics_available(device: str) -> bool:
    return bool(
        torch
        and device.startswith("cuda")
        and torch.cuda.is_available()
    )


def _bytes_to_mebibytes(value: Optional[int]) -> float:
    if not value:
        return 0.0
    return float(value) / (1024 ** 2)


def _select_device(params: "JobParams") -> str:
    import torch

    if params.device == "cpu":
        return "cpu"

    if params.device == "cuda":
        if torch.cuda.is_available():
            return "cuda"
        LOGGER.warning("CUDA requested but not available, falling back to CPU")
        return "cpu"

    if params.device == "auto":
        return "cuda" if torch.cuda.is_available() else "cpu"

    return "cpu"


def _slug(value: str) -> str:
    safe = value.strip().lower().replace(" ", "-")
    return "".join(ch for ch in safe if ch.isalnum() or ch in {"-", "_"}) or "unknown"
