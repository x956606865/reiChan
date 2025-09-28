"""Content-aware double page split prototype for manga scans.

This module hosts the Phase 1 algorithm prototype used by the CLI utility and
unit tests. It focuses on pure in-memory processing so it can later be ported to
Rust without reworking the heuristics.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Iterable

import cv2
import numpy as np


@dataclass(frozen=True)
class SplitConfig:
    """Tunable thresholds for the splitter.

    Values follow the design document defaults and can be tweaked during the
    prototype stage.
    """

    min_aspect_ratio: float = 1.2
    padding_ratio: float = 0.015
    confidence_threshold: float = 0.1
    cover_content_ratio: float = 0.45
    edge_exclusion_ratio: float = 0.12
    min_foreground_ratio: float = 0.01


@dataclass
class SplitResult:
    """Structured result returned by :func:`split_image`."""

    mode: str
    split_x: int | None
    confidence: float
    content_width_ratio: float
    pages: list[np.ndarray]
    metadata: dict[str, Any] = field(default_factory=dict)


def split_image(image: np.ndarray, *, config: SplitConfig | None = None) -> SplitResult:
    """Run the prototype splitter on a single image array.

    Parameters
    ----------
    image:
        BGR image array as loaded by OpenCV (height x width x channels).
    config:
        Optional configuration overrides.
    """

    if config is None:
        config = SplitConfig()

    height, width = image.shape[:2]

    if width < height * config.min_aspect_ratio:
        return SplitResult(
            mode="skip",
            split_x=None,
            confidence=0.0,
            content_width_ratio=0.0,
            pages=[],
            metadata={"reason": "aspect_ratio"},
        )

    mask = _build_foreground_mask(image)
    foreground_ratio = float(mask.mean())

    if foreground_ratio < config.min_foreground_ratio or not mask.any():
        return SplitResult(
            mode="skip",
            split_x=None,
            confidence=0.0,
            content_width_ratio=0.0,
            pages=[],
            metadata={"reason": "no_foreground", "foreground_ratio": foreground_ratio},
        )

    bbox = _compute_bbox(mask)
    bbox_width = bbox[2] - bbox[0]
    bbox_height = bbox[3] - bbox[1]
    content_width_ratio = bbox_width / width
    bbox_height_ratio = bbox_height / height

    metadata: dict[str, Any] = {
        "foreground_ratio": foreground_ratio,
        "bbox": {
            "x": int(bbox[0]),
            "y": int(bbox[1]),
            "width": int(bbox_width),
            "height": int(bbox_height),
        },
    }

    padding_x = max(1, int(config.padding_ratio * width))
    padding_y = max(1, int(config.padding_ratio * height))

    if content_width_ratio < config.cover_content_ratio and bbox_height_ratio > 0.8:
        crop = _crop_region(image, bbox, padding_x, padding_y)
        metadata.update(
            {
                "splitMode": "cover-trim",
                "content_width_ratio": content_width_ratio,
                "bbox_height_ratio": bbox_height_ratio,
            }
        )
        return SplitResult(
            mode="cover-trim",
            split_x=None,
            confidence=1.0,
            content_width_ratio=content_width_ratio,
            pages=[crop],
            metadata=metadata,
        )

    split_x, confidence, projection_meta = _locate_split(mask, config)
    metadata.update(projection_meta)

    if split_x is None or confidence < config.confidence_threshold:
        split_x = width // 2
        mode = "fallback-center"
        confidence = max(confidence, 0.0)
    else:
        mode = "split"

    pages = _extract_pages(image, mask, split_x, padding_x, padding_y)
    metadata.update(
        {
            "splitMode": mode,
            "split_x": int(split_x),
            "confidence": float(confidence),
            "content_width_ratio": content_width_ratio,
        }
    )

    return SplitResult(
        mode=mode,
        split_x=int(split_x),
        confidence=float(confidence),
        content_width_ratio=content_width_ratio,
        pages=pages,
        metadata=metadata,
    )


def _build_foreground_mask(image: np.ndarray) -> np.ndarray:
    """Construct a boolean foreground mask using adaptive thresholding."""

    if image.ndim == 3:
        gray = cv2.cvtColor(image, cv2.COLOR_BGR2GRAY)
    else:
        gray = image.copy()

    blurred = cv2.GaussianBlur(gray, (5, 5), sigmaX=0)
    clahe = cv2.createCLAHE(clipLimit=2.0, tileGridSize=(8, 8))
    equalized = clahe.apply(blurred)

    _, binary = cv2.threshold(
        equalized,
        0,
        255,
        cv2.THRESH_BINARY_INV + cv2.THRESH_OTSU,
    )

    kernel = cv2.getStructuringElement(cv2.MORPH_RECT, (5, 5))
    opened = cv2.morphologyEx(binary, cv2.MORPH_OPEN, kernel, iterations=1)
    cleaned = cv2.morphologyEx(opened, cv2.MORPH_CLOSE, kernel, iterations=1)

    return cleaned > 0


def _compute_bbox(mask: np.ndarray) -> tuple[int, int, int, int]:
    """Calculate tight bounding box (x_min, y_min, x_max_plus1, y_max_plus1)."""

    ys, xs = np.where(mask)
    x_min = int(xs.min())
    x_max = int(xs.max()) + 1
    y_min = int(ys.min())
    y_max = int(ys.max()) + 1
    return (x_min, y_min, x_max, y_max)


def _crop_region(
    image: np.ndarray,
    bbox: tuple[int, int, int, int],
    padding_x: int,
    padding_y: int,
) -> np.ndarray:
    """Crop a region with safety padding, clipping to image boundaries."""

    height, width = image.shape[:2]
    x_min, y_min, x_max, y_max = bbox

    x0 = max(x_min - padding_x, 0)
    x1 = min(x_max + padding_x, width)
    y0 = max(y_min - padding_y, 0)
    y1 = min(y_max + padding_y, height)

    return image[y0:y1, x0:x1].copy()


def _locate_split(mask: np.ndarray, config: SplitConfig) -> tuple[int | None, float, dict[str, Any]]:
    """Find the best split line via projection analysis."""

    height, width = mask.shape
    projection = mask.sum(axis=0).astype(np.float32)

    if projection.max() <= 0:
        return None, 0.0, {"confidence": 0.0}

    sigma = max(width / 200.0, 1.0)
    smoothed = cv2.GaussianBlur(
        projection.reshape(1, -1),
        ksize=(0, 0),
        sigmaX=sigma,
        borderType=cv2.BORDER_REPLICATE,
    ).reshape(-1)

    edge_margin = int(width * config.edge_exclusion_ratio)
    edge_margin = max(edge_margin, 5)

    if edge_margin * 2 >= width:
        return None, 0.0, {"confidence": 0.0}

    search_slice = smoothed[edge_margin : width - edge_margin]
    if search_slice.size == 0:
        return None, 0.0, {"confidence": 0.0}

    candidates = _collect_valleys(smoothed, edge_margin, width - edge_margin)

    if not candidates:
        idx = int(np.argmin(search_slice)) + edge_margin
        candidates = [idx]

    cumulative = np.cumsum(smoothed)
    total = cumulative[-1]
    max_val = float(search_slice.max())

    best_idx = candidates[0]
    best_score = float("inf")

    for idx in candidates:
        valley_value = smoothed[idx]
        left_ratio = float(cumulative[idx]) / (total + 1e-6)
        balance_score = abs(left_ratio - 0.5)
        depth_score = valley_value / (max_val + 1e-6)
        score = balance_score + 0.1 * depth_score
        if score < best_score:
            best_score = score
            best_idx = idx

    confidence = (max_val - smoothed[best_idx]) / (max_val + 1e-6)

    left_mass = float(cumulative[best_idx])
    right_mass = float(total - left_mass)
    imbalance = abs(left_mass - right_mass) / (total + 1e-6)

    metadata = {
        "projection_imbalance": imbalance,
        "projection_edge_margin": edge_margin,
        "projection_total_mass": total,
    }

    return int(best_idx), float(confidence), metadata


def _collect_valleys(data: np.ndarray, start: int, end: int) -> list[int]:
    """Return indices of local minima within [start, end)."""

    valleys: list[int] = []
    for idx in range(max(start, 1), min(end, data.size - 1)):
        if data[idx] <= data[idx - 1] and data[idx] <= data[idx + 1]:
            valleys.append(idx)
    return valleys


def _extract_pages(
    image: np.ndarray,
    mask: np.ndarray,
    split_x: int,
    padding_x: int,
    padding_y: int,
) -> list[np.ndarray]:
    """Slice the input image into right/left pages with padding."""

    height, width = mask.shape

    split_x = int(np.clip(split_x, 1, width - 1))

    right = _crop_region(
        image,
        _compute_region_bbox(mask, split_x, width),
        padding_x,
        padding_y,
    )
    left = _crop_region(
        image,
        _compute_region_bbox(mask, 0, split_x),
        padding_x,
        padding_y,
    )

    return [right, left]


def _compute_region_bbox(mask: np.ndarray, x_start: int, x_end: int) -> tuple[int, int, int, int]:
    """Compute bounding box for a vertical slice of the mask."""

    slice_mask = mask[:, x_start:x_end]
    if not slice_mask.any():
        return (x_start, 0, x_end, mask.shape[0])

    ys, xs = np.where(slice_mask)
    x_min = x_start + int(xs.min())
    x_max = x_start + int(xs.max()) + 1
    y_min = int(ys.min())
    y_max = int(ys.max()) + 1
    return (x_min, y_min, x_max, y_max)


def iter_supported_images(root: Path) -> Iterable[Path]:
    """Yield image files under ``root`` that the CLI can process."""

    from pathlib import Path

    if root.is_file():
        if _is_supported_image(root):
            yield root
        return

    for path in sorted(root.rglob("*")):
        if path.is_file() and _is_supported_image(path):
            yield path


def _is_supported_image(path: Path) -> bool:
    return path.suffix.lower() in {".png", ".jpg", ".jpeg", ".webp"}


__all__ = [
    "SplitConfig",
    "SplitResult",
    "split_image",
    "iter_supported_images",
]
