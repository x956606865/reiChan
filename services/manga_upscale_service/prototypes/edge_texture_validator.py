"""Prototype script for validating edge/texture-based dark margin detection.

Usage example:
    python edge_texture_validator.py --input /path/to/page.png --output-debug debug.png

The script highlights detected left/right margin spans and potential gutter
(split) line leveraging gradient and local entropy metrics described in
``docs/manga-split-dark-margin-strategies.md``.
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

import cv2
import numpy as np


@dataclass
class EdgeTextureConfig:
    gamma: float = 1.0
    gaussian_kernel: int = 5
    entropy_window: int = 15
    entropy_bins: int = 32
    white_threshold: float = 0.45
    left_search_ratio: float = 0.18
    right_search_ratio: float = 0.18
    center_search_ratio: float = 0.3
    min_margin_ratio: float = 0.025
    center_max_ratio: float = 0.06
    score_weights: tuple[float, float, float] = (0.4, 0.35, 0.25)


@dataclass
class MarginRegion:
    start_x: int
    end_x: int
    mean_score: float
    confidence: float


@dataclass
class EdgeTextureResult:
    width: int
    height: int
    left_margin: Optional[MarginRegion]
    right_margin: Optional[MarginRegion]
    center_band: Optional[MarginRegion]
    notes: dict[str, float]

    def to_json(self) -> str:
        payload = {
            "width": self.width,
            "height": self.height,
            "left_margin": asdict(self.left_margin) if self.left_margin else None,
            "right_margin": asdict(self.right_margin) if self.right_margin else None,
            "center_band": asdict(self.center_band) if self.center_band else None,
            "notes": self.notes,
        }
        return json.dumps(payload, indent=2)


def _load_image(path: Path) -> np.ndarray:
    image = cv2.imread(str(path), cv2.IMREAD_COLOR)
    if image is None:
        raise FileNotFoundError(f"Failed to load image: {path}")
    return image


def _apply_gamma(gray: np.ndarray, gamma: float) -> np.ndarray:
    if gamma == 1.0:
        return gray
    inv = 1.0 / max(gamma, 1e-6)
    normalized = np.clip(gray / 255.0, 0.0, 1.0)
    corrected = np.power(normalized, inv)
    return np.uint8(np.clip(corrected * 255.0, 0, 255))


def _compute_entropy(gray: np.ndarray, window: int, bins: int) -> np.ndarray:
    height, width = gray.shape
    pad = window // 2
    padded = cv2.copyMakeBorder(gray, 0, 0, pad, pad, cv2.BORDER_REFLECT)
    ent = np.zeros(width, dtype=np.float32)
    hist_bins = np.linspace(0, 256, bins + 1, dtype=np.float32)

    for x in range(width):
        roi = padded[:, x : x + window]
        hist, _ = np.histogram(roi, bins=hist_bins)
        probs = hist.astype(np.float32)
        total = float(probs.sum())
        if total <= 0.0:
            ent[x] = 0.0
            continue
        probs /= total
        entrop = -np.sum(probs * np.log2(probs + 1e-12))
        ent[x] = entrop

    return ent


def _normalize(arr: np.ndarray) -> np.ndarray:
    arr = arr.astype(np.float32)
    min_val = float(arr.min())
    max_val = float(arr.max())
    if max_val - min_val < 1e-6:
        return np.zeros_like(arr, dtype=np.float32)
    return (arr - min_val) / (max_val - min_val)


def _find_margin(scores: np.ndarray, threshold: float, min_width: int, direction: str) -> Optional[MarginRegion]:
    if direction == "left":
        run_end = 0
        while run_end < scores.size and scores[run_end] <= threshold:
            run_end += 1
        if run_end >= min_width:
            segment = scores[:run_end]
            mean_score = float(segment.mean())
            confidence = float(1.0 - np.clip(mean_score / (threshold + 1e-5), 0.0, 1.0))
            return MarginRegion(0, run_end - 1, mean_score, confidence)
        return None

    if direction == "right":
        run_start = scores.size - 1
        while run_start >= 0 and scores[run_start] <= threshold:
            run_start -= 1
        run_start += 1
        width = scores.size - run_start
        if width >= min_width:
            segment = scores[run_start:]
            mean_score = float(segment.mean())
            confidence = float(1.0 - np.clip(mean_score / (threshold + 1e-5), 0.0, 1.0))
            return MarginRegion(run_start, scores.size - 1, mean_score, confidence)
        return None

    raise ValueError(f"Unsupported direction: {direction}")


def _find_center_band(scores: np.ndarray, threshold: float, max_width: int) -> Optional[MarginRegion]:
    best_region: Optional[MarginRegion] = None
    run_start = None

    for idx, value in enumerate(scores):
        if value <= threshold:
            if run_start is None:
                run_start = idx
        elif run_start is not None:
            run_end = idx - 1
            width = run_end - run_start + 1
            if width <= max_width:
                segment = scores[run_start : run_end + 1]
                mean_score = float(segment.mean())
                confidence = float(1.0 - np.clip(mean_score / (threshold + 1e-5), 0.0, 1.0))
                candidate = MarginRegion(run_start, run_end, mean_score, confidence)
                if best_region is None or candidate.confidence > best_region.confidence:
                    best_region = candidate
            run_start = None

    if run_start is not None:
        run_end = scores.size - 1
        width = run_end - run_start + 1
        if width <= max_width:
            segment = scores[run_start : run_end + 1]
            mean_score = float(segment.mean())
            confidence = float(1.0 - np.clip(mean_score / (threshold + 1e-5), 0.0, 1.0))
            candidate = MarginRegion(run_start, run_end, mean_score, confidence)
            if best_region is None or candidate.confidence > (best_region.confidence if best_region else -1):
                best_region = candidate

    return best_region


def analyze_image(image: np.ndarray, config: EdgeTextureConfig) -> tuple[EdgeTextureResult, np.ndarray]:
    height, width = image.shape[:2]
    gray = cv2.cvtColor(image, cv2.COLOR_BGR2GRAY)
    gray = _apply_gamma(gray, config.gamma)
    blurred = cv2.GaussianBlur(gray, (config.gaussian_kernel, config.gaussian_kernel), 0)

    grad_x = cv2.Sobel(blurred, cv2.CV_32F, 1, 0, ksize=3)
    grad_y = cv2.Sobel(blurred, cv2.CV_32F, 0, 1, ksize=3)
    grad_mag = cv2.magnitude(grad_x, grad_y)

    grad_mean = grad_mag.mean(axis=0)
    grad_var = grad_mag.var(axis=0)
    entropy = _compute_entropy(blurred, config.entropy_window, config.entropy_bins)

    grad_mean_norm = _normalize(grad_mean)
    grad_var_norm = _normalize(grad_var)
    entropy_norm = _normalize(entropy)

    w1, w2, w3 = config.score_weights
    white_score = (
        (1.0 - grad_mean_norm) * w1
        + (1.0 - grad_var_norm) * w2
        + (1.0 - entropy_norm) * w3
    )
    white_score = np.clip(white_score, 0.0, 1.0)

    left_limit = max(1, int(width * config.left_search_ratio))
    right_start = max(0, width - int(width * config.right_search_ratio))
    center_start = max(0, int(width * (0.5 - config.center_search_ratio / 2)))
    center_end = min(width, int(width * (0.5 + config.center_search_ratio / 2)))

    min_margin_width = max(3, int(width * config.min_margin_ratio))
    center_max_width = max(3, int(width * config.center_max_ratio))

    left_region = _find_margin(white_score[:left_limit], config.white_threshold, min_margin_width, "left")
    right_region = _find_margin(white_score[right_start:], config.white_threshold, min_margin_width, "right")
    if right_region:
        right_region = MarginRegion(right_region.start_x + right_start, right_region.end_x + right_start, right_region.mean_score, right_region.confidence)

    center_region = _find_center_band(white_score[center_start:center_end], config.white_threshold, center_max_width)
    if center_region:
        center_region = MarginRegion(center_region.start_x + center_start, center_region.end_x + center_start, center_region.mean_score, center_region.confidence)

    notes = {
        "left_limit": float(left_limit),
        "right_start": float(right_start),
        "center_start": float(center_start),
        "center_end": float(center_end),
        "white_threshold": float(config.white_threshold),
    }

    debug_image = _build_debug_image(image, white_score, left_region, right_region, center_region)
    result = EdgeTextureResult(width, height, left_region, right_region, center_region, notes)
    return result, debug_image


def _build_debug_image(
    image: np.ndarray,
    scores: np.ndarray,
    left: Optional[MarginRegion],
    right: Optional[MarginRegion],
    center: Optional[MarginRegion],
) -> np.ndarray:
    overlay = image.copy()
    height, width = image.shape[:2]

    def draw_region(region: MarginRegion, color: tuple[int, int, int]) -> None:
        cv2.rectangle(overlay, (region.start_x, 0), (region.end_x, height - 1), color, 2)

    if left:
        draw_region(left, (0, 255, 0))
    if right:
        draw_region(right, (255, 128, 0))
    if center:
        draw_region(center, (0, 0, 255))
        cv2.line(overlay, ((center.start_x + center.end_x) // 2, 0), ((center.start_x + center.end_x) // 2, height - 1), (0, 0, 255), 2)

    score_plot_height = 120
    normalized = _normalize(scores)
    plot = np.zeros((score_plot_height, width), dtype=np.uint8)
    for x in range(width):
        value = normalized[x]
        column_height = int(value * (score_plot_height - 1))
        plot[score_plot_height - column_height - 1 :, x] = np.uint8(255 * (1.0 - value))

    plot_colored = cv2.applyColorMap(plot, cv2.COLORMAP_VIRIDIS)
    combined = np.vstack([overlay, plot_colored])
    return combined


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Validate edge/texture based dark margin detection.")
    parser.add_argument("--input", required=True, type=Path, help="Path to the source image file")
    parser.add_argument("--output-debug", type=Path, help="Optional path to save the debug visualization")
    parser.add_argument("--show", action="store_true", help="Display the visualization window")
    parser.add_argument("--gamma", type=float, default=None, help="Override gamma correction factor")
    parser.add_argument("--threshold", type=float, default=None, help="Override white-score threshold")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    config = EdgeTextureConfig()
    if args.gamma is not None:
        config.gamma = args.gamma
    if args.threshold is not None:
        config.white_threshold = args.threshold

    image = _load_image(args.input)
    result, debug_image = analyze_image(image, config)
    print(result.to_json())

    if args.output_debug:
        args.output_debug.parent.mkdir(parents=True, exist_ok=True)
        cv2.imwrite(str(args.output_debug), debug_image)

    if args.show:
        cv2.imshow("edge-texture", debug_image)
        cv2.waitKey(0)
        cv2.destroyAllWindows()


if __name__ == "__main__":
    main()
